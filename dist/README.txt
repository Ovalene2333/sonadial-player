SONADIAL PLAYER / 0.1
=====================

TUI 极简外壳 + 高清发光频谱 / 圆心能量环。
Rust + egui，本地原生播放。

运行
----
双击 SonadialPlayer.exe。release 版不会弹出控制台窗口。
首次点击窗口后，键盘快捷键才会生效。

播放
----
- 把音频文件拖进窗口。
- 或点击左侧 “[ add ]”。
- 也可以把音频文件拖到 SonadialPlayer.exe 上，启动后自动播放。
- 支持：mp3 / flac / wav / ogg / m4a / aac / opus / alac。

下载
----
- 点击左侧 “[ download ]”，粘贴视频链接或 BV 号；目前接入 B 站，入口
  与界面已按多源方式命名。
- 下载与转码在后台线程执行，不阻塞界面，可随时取消。
- 下载为 m4a/AAC，完成后直接加入播放列表并由 Symphonia 解码播放。
- 需要系统 PATH 中有 ffmpeg；默认保存到用户 Music\SonadialPlayer。
- 播放设备跟随 Windows 默认输出设备；切换默认设备后会自动重开音频流并保持进度。

均衡器
------
- 点击顶部 “EQ” 打开 10 段图形均衡器。
- 频段：31 / 62 / 125 / 250 / 500 / 1k / 2k / 4k / 8k / 16k Hz，范围 ±12 dB。
- 支持启用开关、自定义增益和 FLAT / POP / ROCK / CLASS / JAZZ / ELEC /
  VOCAL / BASS / TREBLE / DANCE 预设。
- EQ 在音频回调中实时处理，设置会持久化。

快捷键
------
SPACE              播放 / 暂停
CTRL + O           添加文件
LEFT / RIGHT       后退 / 前进 5 秒
CTRL + LEFT/RIGHT  上一首 / 下一首
ESC                关闭设置面板

界面
----
业务 UI 是 TUI 风格：纯色背景、等宽字体、字符进度条、行号选歌。
内嵌 Sarasa Mono SC 作为 TUI 字体；中文标题和说明使用
Fusion Pixel Font 12px 等宽中文子集，英文与缺字回退到 Sarasa。
频谱和圆心能量环跟随主题着色：默认黑白灰，也可选低饱和度配色，
保留明暗层次与轻微光晕。
背景另有一层像素 LED 频谱：低频在两侧、高频在中间，左右两半交错取样，
纵向铺满整个舞台（顶部标签叠在其上），小格高度固定为 12px，左右各留
12px 空白；频率标签贴底。
VISUALS 中开启 CHAR STREAM 后，改用成熟的 ASCII 密度梯度
`" .:-=+*#%@"`（由暗到亮）表示亮度，播放时字符会缓慢流动。
暂停或停止播放时，频谱和圆心能量环会慢速淡出；恢复播放时慢速淡入。
SYS 面板中可切换 12 套主题（四黑四白三彩一渐变）：MONO / PAPER / SLATE /
MOSS / DUSK / SAND / FOG / ROSE / BLUSH / MINT / AMBER / AURORA。
AURORA 为渐变背景；BLUSH、MINT、AMBER 为彩色主题。可单独开关背景频谱、
圆频谱、圆心跳动，并调节圆频谱尺寸。
底部音量条在当前曲目上显示为 TRK，每首歌的音量会单独记住；
没有当前曲目时调音量则修改默认音量。
算法参数按原 Python 项目 1:1 移植：
- 1024 点 FFT @ 44.1kHz，Hann 窗，100 根非线性分箱（40Hz–8kHz）
- 频谱高度 0.6 新值 + 0.4 旧值平滑
- 低频鼓点检测（28–210Hz，宽带 8kHz，210–2kHz 轻跳）
- energy / bass / mid / treble 平滑，kick 包络与圆环冲击波
- 圆心圆环的扩张、光晕、主环、内核亮度
分析输入取左声道，44.1kHz；不再使用原版橙色和仪表盘外壳。

数据位置
--------
播放列表、默认/曲目音量、EQ、主题与显示设置：%APPDATA%\SonadialPlayer\state.json
崩溃日志：      %APPDATA%\SonadialPlayer\crash.log
下载目录：      %USERPROFILE%\Music\SonadialPlayer

当前范围
--------
已完成：本地播放、解码、进度/跳转、音量、播放列表、循环/随机、
TUI 外壳、高清频谱、背景 LED 频谱、圆心能量环、拖放、
8 套低饱和度主题、频谱显示开关、圆频谱尺寸调节、停止渐隐、
B 站音频下载、10 段实时 EQ、曲目音量记忆。
性能：播放时 30fps 刷新，空闲降到约 6fps；FFT / 视觉缓冲复用，
LED 墙按窗口宽度分列并只绘制点亮单元。
后续：Windows 系统媒体键 / SMTC、系统托盘、安装包与图标。
