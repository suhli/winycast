# Winycast

Winycast 是 Windows 11 的键盘启动器。程序启动后驻留在系统托盘。默认按 **Ctrl + Alt + Space** 唤出或隐藏命令面板。

## 构建与运行

需要 Rust Stable 1.97 或更新版本、`x86_64-pc-windows-msvc` 工具链、Visual Studio 2022 的 C++ 桌面开发组件及 Windows SDK。GPUI 在 Release 构建时使用 SDK 内的 `fxc.exe` 编译 DirectX 着色器。项目通过 `Cargo.lock` 锁定传递依赖。

```powershell
cargo check --workspace --locked
cargo test --workspace --locked
cargo build --release --locked
.\target\release\winycast.exe
```

如果 GPUI 找不到 `fxc.exe`，将环境变量指向已安装的 SDK。下面以 10.0.26100.0 为例。

```powershell
$env:GPUI_FXC_PATH = 'C:\Program Files (x86)\Windows Kits\10\bin\10.0.26100.0\x64\fxc.exe'
cargo build --release --locked
```

命令行参数 `--show` 会在启动时请求打开面板。第二次启动会请求已有实例显示面板。托盘菜单提供打开、设置和退出。设置和历史保存在 `%LOCALAPPDATA%\Winycast\winycast.sqlite3`，测试时可用 `--data-dir <目录>` 指定独立数据目录。

## 操作

- 输入应用名、拼音、拼音首字母或不完整名称即可搜索。例如 `jisuanqi` 和 `jsq` 可以匹配名为「计算器」的索引条目。
- **↑ / ↓** 移动选中项，**Enter** 执行，**Ctrl + K** 打开操作菜单，**Ctrl + D** 收藏或固定。收藏与成功使用记录参与启动器的排序。
- **Tab / Shift + Tab** 在应用、窗口、剪贴板和文件之间切换。运行窗口页会列出当前可切换的顶层窗口。**Ctrl + Space** 收起或展开，**Esc** 从子页面返回，继续按一次隐藏面板。
- **Ctrl + F** 打开文件页，也可从启动器执行「搜索文件」。输入文件名、词组或扩展名，按 **Enter** 用默认应用打开选中的文件。结果来自本机 Windows Search 索引，按修改时间从新到旧排列，最多 100 项。搜索范围由 Windows 的索引设置决定。
- 在搜索框输入 `2+3*4`、`2^8` 或 `=sqrt(81)` 可直接显示结果。按 **Enter** 将结果复制到剪贴板。
- 在设置页用方向键调整选项。可更改主题、主题色、透明度、动画、全局热键、全屏策略、开机启动和剪贴板保留天数。按 **Enter** 录制热键时，支持 Ctrl、Alt、Shift、Win 配合字母、数字、空格或可用的功能键。注册失败会在面板中提示，并保留当前可用热键。
- 剪贴板历史默认暂停。开启后可搜索文本内容，或用「图片」「image」「png」查找图像，选择条目可复制回系统剪贴板。固定项保留到手动取消固定或清空。暂停后仍可访问已保存的历史。
- 在设置中开启「剪贴板图片文字识别」后，Winycast 使用 Windows OCR 在本机处理已有和新增的剪贴板图片。在剪贴板页输入截图里的文字即可找到原图。识别使用 Windows 首选的已安装识别语言，并在可用时额外使用简体中文语言包。此设置默认关闭。关闭时会清除识别索引，清空或过期的图片也会删除对应索引。
- 剪贴板记录尊重 Windows 的历史排除标记和常见密码管理器标记，过滤已知密码管理器来源、形似验证码的数字及部分常见密钥形式。通用文本和截图中的敏感信息无法完全自动识别。历史驻留在本机 SQLite 中。

## 参考与实现

Tinycast 提供了键盘交互、搜索框加列表、底部操作提示、焦点丢失后隐藏等设计参考。Winycast 自行实现 Windows 界面、交互和命令，无 Tinycast 的 Swift 源码、品牌或素材。[Tinycast 界面文档](https://github.com/abue-ammar/tinycast/blob/main/docs/ui.md) · [Tinycast 许可证](https://github.com/abue-ammar/tinycast/blob/main/LICENSE)

使用 Zed 官方发布的 `gpui = "=0.2.2"`，Windows 后端使用 DirectX、DirectWrite 和 DirectComposition。GPUI 暴露原生窗口句柄和文本输入接口。Winycast 的清单声明 Common Controls v6 和 Per-Monitor V2，窗口生命周期由 Win32 事件驱动，通过 HWND 适配层管理。GPUI Kit 的组件和虚拟列表设计用于参考，应用使用自定义 GPUI 组件。[GPUI 0.2.2](https://crates.io/crates/gpui/0.2.2) · [Zed GPUI 源码](https://github.com/zed-industries/zed/tree/main/crates/gpui) · [GPUI Kit](https://github.com/longbridge/gpui-kit)

- `winycast-app` 管理程序生命周期和事件，协调 Tokio 后台任务。
- `winycast-ui` 提供搜索输入、虚拟列表、预览、操作菜单、剪贴板页和设置页。
- `winycast-core` 定义命令、搜索项、设置、拼音匹配及计算器。
- `winycast-platform-windows` 实现全局热键、托盘、单实例、活动显示器定位、原生窗口切换、应用启动、剪贴板读写和 Windows OCR 适配。
- `winycast-storage` 管理 SQLite 设置、排序历史、收藏、剪贴板数据和图片文字索引。
- `winycast-providers` 在可取消的后台任务中扫描应用、搜索窗口及历史数据、查询 Windows Search 文件索引，并索引剪贴板图片文字。

应用扫描读取 Windows AppsFolder 和用户及公共开始菜单。主面板重复使用同一个 HWND 与输入实体。搜索任务按请求取消，发布前核对请求和视图状态。窗口关闭、剪贴板变更和托盘交互由 Windows 派发事件。

## Windows 验收

在 Windows 11（内部版本 26200）上，阶段 0 已实测构建、启动、全局热键唤出和切出后驻留。阶段 1 Release 已以独立数据库启动，扫描到 411 个应用并保持隐藏。一次 6 秒空闲采样记录了 69.1 MiB 工作集，采样期间 CPU 时间增量为 0 ms。长期增长、显示后内存和输入延迟仍待测量。

常规测试覆盖拼音和模糊排序、表达式、IME 的 UTF-16 偏移、SQLite 设置和剪贴板持久化、OCR 索引清理、敏感文本筛选以及 DIB 与 PNG 转换。桌面交互验收在阶段 1 中止，应用启动和窗口切换的交互、真实中文候选、双屏不同 DPI、透明圆角及阴影视觉、全屏输入保护、托盘操作、登录启动及剪贴板真实事件仍待验收。后台进程调用 `--show` 时，Windows 可能拒绝授予前台焦点。

调试构建可用 `--trace-window` 输出索引规模、结果数量、显示状态以及窗口激活耗时，日志不含查询文字或剪贴板内容。`--inspect-window` 让调试面板出现在任务栏，便于被桌面测试工具定位。正式版使用无任务栏工具窗口。

已启用 Windows Search 且索引中包含快捷方式时，可用 `cargo test --workspace --locked reads_files_from_live_windows_index -- --ignored` 复测文件查询和路径解析。

在上述 Windows 11 环境，免安装 Release 已用独立数据库验证后台启动时可为合成图片建立中英文文字索引。安装 Windows 的英语和简体中文 OCR 语言包后，可用 `cargo test --workspace --locked indexes_synthetic_screenshot_with_windows_ocr -- --ignored` 复测图片识别及历史搜索。微软将 `Windows.Media.Ocr` 的桌面支持范围限定为具备包标识的应用。免安装版的可用性取决于本机系统，识别失败时剪贴板历史仍可使用。[微软 Windows OCR 文档](https://learn.microsoft.com/en-us/uwp/api/windows.media.ocr)

建议在包含 100% 和 150% 缩放的双屏 Windows 11 上分别聚焦两个屏幕上的应用，再用热键召唤，检查位置、像素比例、边角和候选窗口的位置。用微软拼音输入，检查拼写中的下划线、高亮候选、空格与 Enter 确认、Escape 取消候选，以及切出再召回后的提交状态。还应分别检查热键切换、点击窗口外部、独占全屏策略、托盘菜单、登录启动、剪贴板真实事件和长时间常驻资源。用测试数据目录验收剪贴板的过期与清空。
