# ADR-20261022-independent-tui-theme：TUI 自管功能配色，普通背景保留终端透明效果

- 状态：生效中
- 日期：2026-10-22
- 决策人：用户
- 适用范围：Nano 自有界面的颜色归属、`NANO_THEME` 与终端背景的边界
- 取代：[ADR-20261022-nano-theme-scope](2026-10-22-nano-theme-scope.md)
- 被取代：无
- 决策来源：2026-10-22 本次会话。用户原话：「不能用ghostty的主题 我们的tui就是我们自己tui的主题」；补充：「如果我的ghostty是半透明，不会被影响」；在明确主体透明、局部高亮受终端控制的边界后确认「好的，请你出spec」。共享配色机制卡选择「采用（推荐）」。

## 决定

- Nano 自有界面的功能配色由一份共享定义提供，不依赖 Ghostty 的 ANSI 调色板或默认前景。既有 `NANO_THEME=dark|light` 扩展到全部自有界面，默认仍为 `dark`。
- 普通背景是有意保留的例外：使用终端默认背景，不铺满固定 RGB 底色；不读取或改写用户的 Ghostty 配置，不通过控制序列修改终端全局默认颜色、调色板或透明度。
- diff、词级强调和焦点/选择等有明确用途的局部背景可以使用 TUI 自有 RGB；不承诺这些局部色块必然透明。它们是否参与透明合成，仍由终端决定。
- 字体、字号、窗口外观、透明度、模糊和终端原生文字选择归终端。内嵌终端保留子程序的显式颜色语义，不用全屏强制替色实现主题统一。
- 本记录约束后续实现；不表示当前代码已经完成主题迁移。

## 依据

- 用户明确否决依赖 Ghostty 主题的方案，并要求保留已有半透明效果。
- 此前的主题裁决将 `NANO_THEME` 限于 diff 与 dock；继续沿用会让正文、边框、状态色仍随终端调色板改变。
- Ghostty 官方配置参考说明：普通背景透明与显式单元格背景分别处理；`background-opacity-cells` 控制后者，macOS 原生全屏另有透明限制。来源：[background-opacity](https://ghostty.org/docs/config/reference#background-opacity)、[background-opacity-cells](https://ghostty.org/docs/config/reference#background-opacity-cells)，2026-10-22 核对；不把终端限制写成 Nano 的透明保证。

## 否决了什么

- 把 Ghostty 主题、调色板注入或用户修改终端配置作为 Nano 正确显示的前提。
- 为了统一颜色而全屏绘制固定背景，遮住用户原有透明背景。
- 为每个面板各建一套配色，或增加主题插件、自动主题探测和设置页。
- 强行重写子程序所有颜色，或承诺所有背景色块都必然透明。

## 接受的代价

- 默认背景由用户终端决定，不能同时保证背景 RGB 完全一致与透明背景不变。用户需手动选择与实际底色合适的 `dark` / `light`；任意壁纸和透明度下的对比度不作无条件保证。
- 三处渲染模块和相关测试需要统一接入共享配色；已有依赖 ANSI 色名的断言需要迁移。
- 显式局部底色和 macOS 原生全屏的透明行为受 Ghostty 控制，Nano 不代用户调整。

## 允许重开的条件

- 用户明确要求由 Nano 接管主背景或引入更多主题能力。
- 真实终端证据表明本边界无法同时维持主要内容可读性与主体透明，需要重新确定取舍。
- 终端协议或 Ghostty 透明合成方式发生可验证变化。

## 关联

- 规格：[2026-10-22-nano-theme-transparency](../specs/2026-10-22-nano-theme-transparency/README.md)。
- 历史决定：[ADR-20261022-nano-theme-scope](2026-10-22-nano-theme-scope.md)；其正文保留为历史。
