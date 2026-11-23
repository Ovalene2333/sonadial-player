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
| `family-ui` | Sarasa Mono SC | 控件、时间、英文标签 |
| `family-title` | 同 `family-pixel`，Fusion Pixel | 列表和底部歌名，使用已注册的中日韩回退字体 |
| `family-pixel` | Fusion Pixel 12px Mono latin | 歌词、搜索框和部分输入框，使用已注册的中日韩回退字体 |
| `font-caption` | 12px | 辅助信息、面板说明文字 |
| `font-button` | 12px | 普通按钮、设置页标签、弹窗关闭符号 |
| `font-library` | 12px | 列表序号、LIBRARY 标签、空列表提示 |
| `font-track-title` | 12px | 列表歌名与右键菜单歌名，使用 Fusion 原生字号 |
| `font-app-title` | 16px | 顶栏应用名 |
| `font-core` | 16px | 圆心时间；小圆盘使用 `font-caption` |
| `font-heading` | 24px | 设置、均衡器、下载等面板标题 |
| `font-dock-title` | 12px | 底部歌名与下一首，使用 Fusion 原生字号；靠像素加粗和亮度强调，不靠字号 |
| `font-dock-control` | 12px | 进度、时间、音量、百分比、播放按钮 |
| `font-pixel-caption` | 12px | 使用 Fusion 的输入内容与占位文字 |
| `font-lyric` / `font-lyric-next` | 24px / 12px | 当前歌词、下一句歌词 |

`font-small/title/large` 定义 12/16/24 的三档基础刻度；各角色引用这些刻度，可以单独覆盖角色。普通控件的字号通过样式 token 控制；`InkBtn.text-size` 用于底部按钮等具有独立角色的实例。

Sarasa ASCII 的字符宽度为字号的一半，字符条应使用偶数字号。`dock-cell-w` 与 `AsciiMeter.cell-w` 都从字号推导；修改底部控制字号时，字符位置、Rust 生成的格数和鼠标映射一起跟随。Fusion 按原生 12px 网格绘制，使用 12px 的整数倍。

底部第一行右侧的 `NEXT` 预告与实际播放规则一致：插队队列优先；顺序模式显示下一首，列表末尾显示 `END OF LIST`；单曲循环显示 `REPEAT ONE`；随机模式在每首歌开始时预先选好下一首（按路径记录，曲目被删除时重新选），预告和实际播放的一定是同一首。

时间标签把当前位置左侧补空格到总时长的宽度（如 ` 0:05 / 11:40`），所以同一首歌播放期间标签宽度和进度条格数保持不变。

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
| `dock-content-fraction` | 0.55 | 底部进度行（进度条 + 时间）占窗口宽度的比例，也是当前歌名的最大宽度 |
| `dock-inset` | 16px | 底部左右留白 |
| `dock-margin` / `dock-row-gap` | 14 / 12px | 按字形计算的视觉间距：上下留白相等，三行之间的间距相等（以中文字形为准；纯英文歌名基线略高，看起来会多约 2px） |
| `dock-title-h/seek-h/controls-h` | 20/18/28px | 三行的布局与点击高度；行内上下留白为 `(行高 - 字号) / 2`，纵向位置由上面的视觉间距反推 |
| `dock-title-y/seek-y/controls-y` | 10/35/54px | 推导结果，不要直接改 |
| `dock-h` | 88px | 由控制行字形下沿加 `dock-margin` 推导；舞台、侧栏、提示框一起跟随 |
| `dock-title-bold` | 1px | 像素加粗：歌名右移 1px 再画一次（点阵终端的双击加粗） |
| `dock-shimmer-period/sweep/half/steps` | 2800ms / 0.6 / 36px / 3 | 播放时歌名上的流光：每个周期前 60% 时间扫过歌名、其余停顿；3 层嵌套裁剪窗口从 `c-text` 逐级提亮到 `c-text-hi`，形成像素阶梯渐变，单侧宽 36px。Rust 只推进相位（暂停时冻结，切歌从左侧重新开始），播放中歌名底色为 `c-text`，为流光留出提亮空间 |
| `dock-cluster-gap` / `dock-next-label-gap` | 24 / 8px | 当前歌名与下一首之间的最小间隔；`NEXT` 标签与下一首歌名的间距 |
| `dock-button-w/stop-w/button-gap` | 80/68/12px | 播放按钮宽度与间距，按钮组整体居中 |
| `dock-volume-prefix-cells/cells` | 5 / 16 | `VOL [` 与音量条的字符数，显示与交互共享 |
| `dock-seek-min/max-cells` | 18 / 180 | 进度条格数上下限；实际格数由 Rust 按 `dock-content-w` 减去时间标签计算 |
| `panel-*` | 见样式表 | 面板内边距、标题、关闭按钮、正文起点 |
| `settings-w/h` / `eq-w/h` / `download-w/h` | 470×440 / 700×420 / 560×420px | 主面板尺寸 |
| `dialog-w/h` / `toast-w/h` | 460×220 / 480×64px | 确认框与提示框尺寸 |
| `theme-columns/gap/row-step` | 4 / 8px / 34px | 主题网格、Rust 分组与滚动内容高度 |
| `menu-*` | 见样式表 | 菜单宽度、标题与行高；带歌词的菜单自动多一行 |

公共尺寸放入 `Style`，组件外观与指针映射放入 `widgets.slint`。页面特有的内容顺序和定位在 `main.slint`，相邻控件优先用布局容器或由 token 推导的位置。

颜色仍由现有主题系统提供，通过 `style.slint` 中的 `Theme` 接口更新。样式表随程序编译嵌入，调整后需要重新构建。

## 视觉检查

设置 `SONADIAL_RENDER_PREVIEW` 为临时输出目录后启动程序，可以用实际 Slint 软件渲染器输出窗口、最小尺寸、最大化、悬停、面板和菜单截图。预览不打开音频设备、不修改用户保存的状态。`geometry.json` 记录布局，`interactions.json` 记录通过真实指针事件触发的进度、音量和播放按钮回调，供临时 Python 脚本核对。
