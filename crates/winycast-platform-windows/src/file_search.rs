use std::{
    collections::HashSet,
    ffi::OsString,
    mem::{offset_of, size_of},
    os::windows::ffi::OsStringExt,
    path::PathBuf,
    ptr::{from_mut, null, null_mut},
};

use anyhow::{Context, Result};
use windows::{
    Win32::System::{
        Com::{CLSCTX_INPROC_SERVER, CoCreateInstance},
        Search::{
            DBACCESSOR_ROWDATA, DBBINDING, DBMEMOWNER_CLIENTOWNED, DBPARAMIO_NOTPARAM,
            DBPART_LENGTH, DBPART_STATUS, DBPART_VALUE, DBSTATUS_S_OK, DBTYPE_WSTR, HACCESSOR,
            IAccessor, ICommandText, IDBCreateCommand, IDBCreateSession, IDBInitialize,
            IDataInitialize, IRowset, MSDAINITIALIZE,
        },
    },
    Win32::UI::Shell::PathCreateFromUrlW,
    core::{GUID, HSTRING, IUnknown, Interface, PCWSTR, PWSTR, w},
};

use crate::shell::Apartment;

const RESULT_LIMIT: usize = 100;
// DBGUID_DEFAULT from oledb.h.
const DEFAULT_DIALECT: GUID = GUID::from_u128(0xc8b521fb_5cf3_11ce_ade5_00aa0044773d);
// INTERNET_MAX_URL_LENGTH plus the terminating NUL.
const URL_CAPACITY: usize = 2_085;
const PATH_CAPACITY: usize = 32_768;

pub fn search_files(query: &str, cancelled: impl Fn() -> bool) -> Result<Vec<PathBuf>> {
    if cancelled() {
        return Ok(Vec::new());
    }

    let Some(sql) = file_query(query) else {
        return Ok(Vec::new());
    };
    let _apartment = Apartment::enter()?;
    let rowset =
        open_rowset(&sql).context("无法查询 Windows Search 索引，请检查 Windows Search 服务")?;
    if cancelled() {
        return Ok(Vec::new());
    }

    let accessor = PathAccessor::new(&rowset)?;
    let mut column = PathColumn {
        status: 0,
        length: 0,
        value: [0; URL_CAPACITY],
    };
    let mut paths = Vec::new();
    let mut seen = HashSet::new();
    let mut path_buffer = [0; PATH_CAPACITY];

    while !cancelled() && paths.len() < RESULT_LIMIT {
        let mut fetched = 0;
        let mut handle = 0;
        // Supplying a slot lets OLE DB return the row handle without allocating an array.
        let mut slot = from_mut(&mut handle);
        unsafe { rowset.GetNextRows(0, 0, &mut fetched, std::slice::from_mut(&mut slot))? };
        if fetched == 0 {
            break;
        }

        let result =
            unsafe { rowset.GetData(handle, accessor.handle, from_mut(&mut column).cast()) };
        // Release every fetched handle, including a row whose data could not be read.
        unsafe { rowset.ReleaseRows(1, &handle, null(), null_mut(), null_mut())? };
        result?;

        if let Some(path) = column.path(&mut path_buffer)
            && seen.insert(path.clone())
        {
            paths.push(path);
        }
    }

    if cancelled() {
        return Ok(Vec::new());
    }
    Ok(paths)
}

fn open_rowset(sql: &str) -> Result<IRowset> {
    unsafe {
        let initializer: IDataInitialize =
            CoCreateInstance(&MSDAINITIALIZE, None, CLSCTX_INPROC_SERVER)?;
        let mut source = None;
        initializer.GetDataSource(
            None::<&IUnknown>,
            CLSCTX_INPROC_SERVER.0,
            w!("Provider=Search.CollatorDSO;Extended Properties='Application=Windows';"),
            &IDBInitialize::IID,
            &mut source,
        )?;
        let source: IDBInitialize = source.context("Windows Search 未返回数据源")?.cast()?;
        source.Initialize()?;

        let session: IDBCreateCommand = source
            .cast::<IDBCreateSession>()?
            .CreateSession(None::<&IUnknown>, &IDBCreateCommand::IID)?
            .cast()?;
        let command: ICommandText = session
            .CreateCommand(None::<&IUnknown>, &ICommandText::IID)?
            .cast()?;
        command.SetCommandText(&DEFAULT_DIALECT, &HSTRING::from(sql))?;

        let mut rows = None;
        command.Execute(
            None::<&IUnknown>,
            &IRowset::IID,
            None,
            None,
            Some(&mut rows),
        )?;
        Ok(rows.context("Windows Search 未返回结果集")?.cast()?)
    }
}

#[repr(C)]
struct PathColumn {
    status: u32,
    length: usize,
    value: [u16; URL_CAPACITY],
}

impl PathColumn {
    fn path(&self, path_buffer: &mut [u16]) -> Option<PathBuf> {
        if self.status != DBSTATUS_S_OK.0 as u32 || self.length % size_of::<u16>() != 0 {
            return None;
        }
        let url_length = self.length / size_of::<u16>();
        let url = self.value.get(..=url_length)?;
        if url[url_length] != 0 || url[..url_length].contains(&0) {
            return None;
        }
        let mut capacity = path_buffer.len() as u32;
        unsafe {
            PathCreateFromUrlW(
                PCWSTR(url.as_ptr()),
                PWSTR(path_buffer.as_mut_ptr()),
                &mut capacity,
                0,
            )
        }
        .ok()?;
        let path_length = path_buffer.iter().position(|&unit| unit == 0)?;
        let path = PathBuf::from(OsString::from_wide(&path_buffer[..path_length]));
        if path.is_absolute() && path.file_name().is_some() {
            Some(path)
        } else {
            None
        }
    }
}

struct PathAccessor {
    accessor: IAccessor,
    handle: HACCESSOR,
}

impl PathAccessor {
    fn new(rowset: &IRowset) -> Result<Self> {
        let accessor: IAccessor = rowset.cast()?;
        let binding = DBBINDING {
            iOrdinal: 1,
            obValue: offset_of!(PathColumn, value),
            obLength: offset_of!(PathColumn, length),
            obStatus: offset_of!(PathColumn, status),
            dwPart: (DBPART_VALUE.0 | DBPART_LENGTH.0 | DBPART_STATUS.0) as u32,
            dwMemOwner: DBMEMOWNER_CLIENTOWNED.0 as u32,
            eParamIO: DBPARAMIO_NOTPARAM.0 as u32,
            cbMaxLen: URL_CAPACITY * size_of::<u16>(),
            wType: DBTYPE_WSTR.0 as u16,
            ..Default::default()
        };
        let mut handle = HACCESSOR::default();
        unsafe {
            accessor.CreateAccessor(
                DBACCESSOR_ROWDATA.0 as u32,
                1,
                &binding,
                size_of::<PathColumn>(),
                &mut handle,
                None,
            )?;
        }
        Ok(Self { accessor, handle })
    }
}

impl Drop for PathAccessor {
    fn drop(&mut self) {
        unsafe {
            let _ = self.accessor.ReleaseAccessor(self.handle, None);
        }
    }
}

fn file_query(input: &str) -> Option<String> {
    let input = input.trim();
    if input.is_empty() || input.encode_utf16().count() > 255 || input.contains('\0') {
        return None;
    }
    let mut sql = format!(
        "SELECT TOP {RESULT_LIMIT} System.ItemUrl FROM SystemIndex \
         WHERE System.ItemUrl LIKE 'file:%' AND System.ItemType <> 'Directory'"
    );
    for term in input.split_whitespace() {
        sql.push_str(" AND System.FileName LIKE '%");
        for ch in term.chars() {
            // Quote SQL literals and match user-entered wildcard characters literally.
            match ch {
                '\'' => sql.push_str("''"),
                '%' => sql.push_str("[%]"),
                '_' => sql.push_str("[_]"),
                '[' => sql.push_str("[[]"),
                _ => sql.push(ch),
            }
        }
        sql.push_str("%'");
    }
    sql.push_str(" ORDER BY System.DateModified DESC");
    Some(sql)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terms_are_matched_literally() {
        let sql = file_query("会议 O'Brien_[1] 50%.pdf").unwrap();
        assert!(sql.contains("System.FileName LIKE '%会议%'"));
        assert!(sql.contains("System.FileName LIKE '%O''Brien[_][[]1]%'"));
        assert!(sql.contains("System.FileName LIKE '%50[%].pdf%'"));
        assert!(file_query("  ").is_none());
        assert!(file_query("bad\0query").is_none());
        assert!(file_query(&"𠮷".repeat(128)).is_none());
    }

    #[test]
    fn cancelled_queries_skip_the_system_index() {
        assert!(search_files(".lnk", || true).unwrap().is_empty());
    }

    #[test]
    fn converts_windows_search_file_urls_to_native_paths() {
        let mut path_buffer = [0; PATH_CAPACITY];
        for (url, expected) in [
            (
                "file:C:/Start Menu/Character Map.lnk",
                r"C:\Start Menu\Character Map.lnk",
            ),
            (
                "file:///C:/Documents/50%25%20off.txt",
                r"C:\Documents\50% off.txt",
            ),
        ] {
            let input: Vec<_> = url.encode_utf16().collect();
            let mut value = [0; URL_CAPACITY];
            value[..input.len()].copy_from_slice(&input);
            let column = PathColumn {
                status: DBSTATUS_S_OK.0 as u32,
                length: input.len() * size_of::<u16>(),
                value,
            };
            assert_eq!(column.path(&mut path_buffer), Some(PathBuf::from(expected)));
        }
    }

    #[test]
    #[ignore = "requires Windows Search and indexed Start Menu shortcuts"]
    fn reads_files_from_live_windows_index() {
        let paths = search_files(".lnk", || false).unwrap();
        assert!(!paths.is_empty());
        assert!(paths.len() <= RESULT_LIMIT);
        assert!(paths.iter().all(|path| {
            path.file_name()
                .is_some_and(|name| name.to_string_lossy().to_ascii_lowercase().contains(".lnk"))
        }));
        assert!(paths.iter().any(|path| path.is_file()));
        eprintln!("Read {} shortcuts from Windows Search", paths.len());
    }
}
