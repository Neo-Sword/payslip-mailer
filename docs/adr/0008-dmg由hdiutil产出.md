# ADR-0008 · dmg 由 hdiutil 产出，不依赖 Finder

日期：2026-11-11 · 状态：accepted

## 背景

`tauri.conf.json` 原本 `bundle.targets = ["app", "dmg"]`。首次跑通 universal 构建时，
`.app` 与双架构二进制都成功（`lipo -archs` → `x86_64 arm64`），**只有 dmg 这一步失败**：

```
Running bundle_dmg.sh
Searching for mounted interstitial disk image using /dev/disk4s...
execution error: "Finder"遇到一个错误：AppleEvent已超时。 (-1712)
Failed running AppleScript
```

原因：Tauri 的 `bundle_dmg.sh`（以及 `create-dmg`）挂载 dmg 后要**用 AppleScript 指挥 Finder**
摆图标、设窗口尺寸。这一步需要 GUI 会话 + 「自动化」TCC 授权；
在后台任务 / CI / SSH 这类没有可应答授权弹窗的环境里，AppleEvent 直接超时。

失败位置也很坑：它发生在构建链的**最后一步**，前面几分钟的编译都白跑，
而报错信息里看不到「需要 GUI」这个线索。

## 决策

**dmg 由 `scripts/make-dmg.sh` 用 `hdiutil` 产出；`tauri.conf.json` 的 targets 只留 `["app"]`。**

- `hdiutil create -volname … -srcfolder … -format UDZO` 是纯命令行的，无 GUI 依赖，同样条件下必成。
- 「拖到 Applications」的引导靠 staging 目录里的一个 `Applications` 符号链接实现，
  功能等价，只是没有图标定位与背景图。
- `tauri.conf.json` 里原来的 `macOS.dmg` 图标坐标配置一并删除，避免与 make-dmg.sh 里的
  create-dmg 参数形成**两处布局真值**。
- `make-dmg.sh` 仍**优先**尝试 create-dmg（有人装了它、且在 GUI 会话里，就得到美化版），
  但失败时自动回退 hdiutil，不再让美化环节有能力杀死整条发布链。

## 为什么不是「修好 AppleScript」

可以让脚本前台跑、手动点授权，但那样发布就变成「必须在有屏幕的机器上、有人守着」的操作。
本产品要长期给一台 HR 管理机出包，可脚本化比 dmg 里那几个图标摆得好看更重要。
美化版 dmg 是**锦上添花**，所以降级为「可选路径 + 自动回退」。

## 顺带修掉的另一个隐患：产物不放 `dist/`

`dist/` 是 vite 的输出目录（`tauri.conf.json` 的 `frontendDist: "../dist"`），
而 vite 构建前会**清空 outDir**。原先 `release.sh` 也把 dmg 输出到 `./dist`，
意味着任何人下一次 `npm run build` 都会把刚出的包静默删掉。
现改为输出到 `./artifacts/`（已加入 `.gitignore`）。

## 后果

- 正面：`bash scripts/release.sh` 在无 GUI 的会话里也能跑完；产物路径不再会被前端构建清掉。
- 负面：默认产出的 dmg 没有美化布局（打开是一个普通文件夹窗口）。
  对内部分发无实质影响；需要美化时装 create-dmg 并在 GUI 终端里跑。
- 事实记录：本机 `create-dmg` **未安装**，所以当前产物就是 hdiutil 版；
  首次产出 `dist/薪邮-0.1.0.dmg` 4.7 MB，
  SHA-256 `319555e7dd1d60f514b147f5186da1fd0109566a601063bf2eaa783884c1623e`。

## 重新评估的触发条件

- 需要给非技术使用者一个「像样」的安装窗口 → 装 create-dmg，并规定发布必须在 GUI 终端执行。
- 若 Tauri 后续版本的 bundler 去掉 AppleScript 依赖 → 把 `dmg` 加回 targets，删掉本脚本的定制分支。
