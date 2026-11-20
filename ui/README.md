# UI 样式速查

`style.slint` 是字体、间距和公共布局尺寸的唯一来源。Slint 组件引用 `Style.*`，Rust 通过 `ui.global::<Style>()` 读取同一组值。所有尺寸采用逻辑像素。

| 文件 | 职责 |
| --- | --- |
| `style.slint` | 字体角色、字号刻度、间距、窗口与面板尺寸；运行时主题颜色接口 |
| `widgets.slint` | 按钮、标签页、主题色块、菜单行、ASCII 字符条的公共样式与交互 |
| `main.slint` | 界面结构、数据绑定、各页面的内容排布 |
| `../src/ui.rs` | 播放器状态；读取已解析的布局和 token，生成显示数据 |

## 字体与字号

| 角色 token | 当前值 | 用途 |
| --- | --- | --- |
| `family-ui` | Sarasa Mono SC | 控件、时间、元信息、英文标签 |
| `family-title` | 同 `family-pixel`，Fusion Pixel | 列表和底部歌名，使用已注册的中日韩回退字体 |
| `family-pixel` | Fusion Pixel 12px Mono latin | 歌词、搜索框和部分输入框，使用已注册的中日韩回退字体 |
| `font-caption` | 12px | 格式、采样率、设备、播放模式、辅助信息 |
| `font-button` | 12px | 普通按钮、设置页标签 |
| `font-library` | 14px | 列表序号、LIBRARY 标签、空列表提示 |
| `font-track-title` | 12px | 列表歌名，使用 Fusion 原生字号 |
| `font-app-title` | 16px | 顶栏应用名、关闭符号 |
| `font-core` | 16px | 圆心时间；小圆盘使用 `font-caption` |
| `font-heading` | 24px | 设置、均衡器、下载等面板标题 |
| `font-dock-title` | 12px | 底部歌名，使用 Fusion 原生字号 |
| `font-dock-control` | 14px | 进度、时间、音量、百分比、播放按钮 |
| `font-pixel-caption` | 12px | 使用 Fusion 的输入内容与占位文字 |
| `font-lyric` / `font-lyric-next` | 24px / 12px | 当前歌词、下一句歌词 |

`font-small/body/title/large` 定义 12/14/16/24 的基础刻度；各角色引用这些刻度，可以单独覆盖角色。普通控件的字号通过样式 token 控制；`InkBtn.text-size` 用于底部按钮等具有独立角色的实例。

Sarasa ASCII 的字符宽度为字号的一半，字符条应使用偶数字号。`dock-cell-w` 与 `AsciiMeter.cell-w` 都从字号推导；修改底部控制字号时，字符位置、Rust 生成的格数和鼠标映射一起跟随。Fusion 按原生 12px 网格绘制，使用 12px 的整数倍。

## 间距与布局

| token / 分组 | 当前值 | 关联行为 |
| --- | --- | --- |
| `stroke` | 1px | 分隔线、边框 |
| `space-xxs/xs/tight/sm/md/lg/xl/2xl` | 2/4/6/8/12/16/20/24px | 公共间距刻度 |
| `control-h` / `field-h` / `line-h` | 22/20/18px | 普通按钮、输入框、短信息行 |
| `row-h` | 26px | 列表行高、滚动范围、自动滚动、右键菜单坐标 |
| `window-w/h` | 1180×760px | 初始窗口尺寸 |
| `window-min-w/h` | 920×600px | 最小窗口尺寸 |
| `topbar-h` / `sidebar-w` | 44px / 360px | 舞台边界随顶栏和侧栏计算 |
| `library-*` / `scroll-*` | 见样式表 | 搜索、按钮、列表列宽、滚动条 |
| `dock-content-fraction` | 0.55 | 底部歌名和进度行占窗口宽度的比例 |
| `dock-inset` | 16px | 底部左右留白 |
| `dock-title-h` | 24px | 歌名行的布局高度，独立于字体字号 |
| `dock-title-y/meta-y/seek-y/controls-y` | 12/38/54/82px | 底部从标题到控制行的纵向节奏，由前一行高度与间距推导 |
| `dock-controls-h` | 30px | 音量和播放按钮共用的行高与点击高度 |
| `dock-h` | 124px | 由控制行下沿和底部留白推导；舞台、侧栏、提示框一起跟随 |
| `dock-button-w/stop-w/button-gap` | 80/68/12px | 播放按钮宽度与间距，按钮组整体居中 |
| `dock-volume-cells` | 16 | 音量条的字符数，显示与交互共享 |
| `panel-*` | 见样式表 | 面板内边距、标题、关闭按钮、正文起点 |
| `settings-w/h` / `eq-w/h` / `download-w/h` | 470×440 / 700×420 / 560×420px | 主面板尺寸 |
| `dialog-w/h` / `toast-w/h` | 460×220 / 480×64px | 确认框与提示框尺寸 |
| `theme-columns/gap/row-step` | 4 / 8px / 34px | 主题网格、Rust 分组与滚动内容高度 |
| `menu-*` | 见样式表 | 菜单宽度、标题与行高；带歌词的菜单自动多一行 |

公共尺寸放入 `Style`，组件外观与指针映射放入 `widgets.slint`。页面特有的内容顺序和定位在 `main.slint`，相邻控件优先用布局容器或由 token 推导的位置。

颜色仍由现有主题系统提供，通过 `style.slint` 中的 `Theme` 接口更新。样式表随程序编译嵌入，调整后需要重新构建。

## 视觉检查

设置 `SONADIAL_RENDER_PREVIEW` 为临时输出目录后启动程序，可以用实际 Slint 软件渲染器输出窗口、最小尺寸、最大化、悬停、面板和菜单截图。预览不打开音频设备、不修改用户保存的状态。`geometry.json` 记录布局，`interactions.json` 记录通过真实指针事件触发的进度、音量和播放按钮回调，供临时 Python 脚本核对。
