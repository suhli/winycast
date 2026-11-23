use std::{cell::RefCell, path::Path, rc::Rc};

use anyhow::{Context as _, Result, ensure};
use async_channel::Receiver;
use gpui::{
    AnyWindowHandle, App, AppContext, Application, Bounds, Keystroke, WindowBounds, WindowHandle,
    WindowOptions, px, size,
};
use winycast_core::{NoteDraft, NoteVersion, Page, Settings, markdown_preview};
use winycast_storage::Storage;
use winycast_ui::{CommandPalette, Intent, PANEL_HEIGHT, PANEL_WIDTH};

#[test]
#[ignore = "requires the Windows GPUI graphics backend"]
fn note_editor_draws_and_preserves_draft_through_preview_and_save() -> Result<()> {
    let outcome = Rc::new(RefCell::new(None));
    let result = outcome.clone();
    Application::new().run(move |cx| {
        *result.borrow_mut() = Some(exercise_note_editor(cx));
        cx.quit();
    });
    outcome
        .borrow_mut()
        .take()
        .context("GPUI test did not run")?
}

fn exercise_note_editor(cx: &mut App) -> Result<()> {
    let (intents, events) = async_channel::unbounded();
    let window = cx.open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
                None,
                size(px(PANEL_WIDTH as f32), px(PANEL_HEIGHT as f32)),
                cx,
            ))),
            show: false,
            focus: false,
            ..Default::default()
        },
        |_, cx| {
            cx.new(|cx| {
                CommandPalette::new(
                    intents,
                    Settings {
                        animations: false,
                        ..Default::default()
                    },
                    cx,
                )
            })
        },
    )?;
    window.update(cx, |palette, window, cx| {
        palette.open_page(Page::Notes, window, cx);
        palette.new_note(window, cx);
    })?;
    paste(window, "交付记录", cx)?;
    press(window, "tab", cx)?;
    let original = "# 交付\n\n> **正文** 和 `code`\n\n- [x] 测试";
    paste(window, original, cx)?;
    window.update(cx, |palette, window, cx| {
        palette.hidden(cx);
        palette.show(None, window, cx);
    })?;
    press(window, "ctrl-p", cx)?;
    let (request, source) = std::iter::from_fn(|| events.try_recv().ok())
        .find_map(|intent| match intent {
            Intent::RenderNote { request, markdown } => Some((request, markdown)),
            _ => None,
        })
        .context("preview should dispatch the draft")?;
    ensure!(source == original, "preview lost part of the draft");
    window.update(cx, |palette, _, cx| {
        palette.note_preview(request, markdown_preview(&source), cx);
    })?;
    press(window, "ctrl-s", cx)?;
    let storage = Storage::open(Path::new(":memory:"))?;
    let (session, version, draft) = pending_save(&events)?;
    ensure!(version.is_none() && draft.title == "交付记录" && draft.markdown == original);
    let saved = storage.save_note(version, draft)?;

    press(window, "ctrl-p", cx)?;
    press(window, "tab", cx)?;
    press(window, "ctrl-a", cx)?;
    let revised = "保存过程中继续输入";
    paste(window, revised, cx)?;
    window.update(cx, |palette, window, cx| {
        palette.note_saved(session, Ok(saved.clone()), window, cx);
    })?;
    press(window, "escape", cx)?;
    press(window, "escape", cx)?;
    press(window, "ctrl-s", cx)?;
    let (_, version, draft) = pending_save(&events)?;
    ensure!(version == Some(saved.version) && draft.markdown == revised);
    let updated = storage.save_note(version, draft)?;
    ensure!(storage.note(saved.version.id)? == Some(updated));
    Ok(())
}

fn press(window: WindowHandle<CommandPalette>, chord: &str, cx: &mut App) -> Result<()> {
    let key = Keystroke::parse(chord)?;
    AnyWindowHandle::from(window).update(cx, |_, window, cx| {
        let _ = window.draw(cx);
        window.dispatch_keystroke(key, cx)
    })?;
    Ok(())
}

fn paste(window: WindowHandle<CommandPalette>, text: &str, cx: &mut App) -> Result<()> {
    window.update(cx, |palette, window, cx| {
        let focus = window.focused(cx).context("editor has no focused field")?;
        palette.paste_input(&focus, text.into(), window, cx);
        Ok(())
    })?
}

fn pending_save(events: &Receiver<Intent>) -> Result<(u64, Option<NoteVersion>, NoteDraft)> {
    std::iter::from_fn(|| events.try_recv().ok())
        .find_map(|intent| match intent {
            Intent::SaveNote {
                session,
                version,
                draft,
            } => Some((session, version, draft)),
            _ => None,
        })
        .context("save should dispatch the draft")
}
