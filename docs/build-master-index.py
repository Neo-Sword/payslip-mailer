#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""
薪邮 Payslip Mailer · 总纲.html 生成器

形态参考 ../AgentTeam/总纲.html：单文件自包含、离线可开、上半看板 / 下半分类登记、
跨分类全文检索 + 分类内检索 + 折叠、右下「最新更新」抽屉、末尾维护规范与履历。

与参考实现的一处关键差别（本项目刻意如此）：
    任务卡状态 **不手写在总纲里**，而是每次生成时从 docs/TASKBOARD.md 的表格读取；
    spec / ADR 条目由目录扫描自动生成。总纲只做导航与看板，不产生新决策 ——
    决策看 ADR，做成什么样看 spec，当前状态看 TASKBOARD，冲突一律以源文档为准。

用法：
    python3 docs/build-master-index.py
    # 生成 <仓库根>/总纲.html；所有内部链接自动 URL 编码并校验存在性，断链会在终端报警。

新增 spec / ADR 后：在下方 SPEC_NOTES / ADR_NOTES 补一句核心口径，重跑即可；
未补说明的条目会标注「待补核心口径」，不会静默漏掉。
"""

from __future__ import annotations

import html as _html
import re
from datetime import datetime
from pathlib import Path
from urllib.parse import quote

ROOT = Path(__file__).resolve().parent.parent
OUT = ROOT / "总纲.html"
VERSION = "V1.1"
BUILT = "2026-11-12"       # 总纲建立日
UPDATED = "2026-11-12"     # 本次状态基线（与 TASKBOARD 同步）

WARN: list[str] = []


def esc(s) -> str:
    return _html.escape(str(s), quote=True)


def href(rel: str, anchor: str = "") -> str:
    """相对路径 → 可直接写进 file:// 页面的 URL（中文自动编码），顺便查断链。"""
    p = ROOT / rel
    if not p.exists() and p != OUT:  # OUT 是本脚本正在生成的自指页面
        WARN.append(rel)
    return quote(rel, safe="/") + ("#" + anchor if anchor else "")


def a(rel: str, text: str, anchor: str = "", cls: str = "") -> str:
    c = ' class="' + cls + '"' if cls else ""
    return '<a href="' + href(rel, anchor) + '"' + c + ">" + esc(text) + "</a>"


def stamp(rel: str) -> str:
    p = ROOT / rel
    if not p.exists():
        return BUILT
    return datetime.fromtimestamp(p.stat().st_mtime).strftime("%Y-%m-%d")


# ─────────────────────────────────────────────────────────────────────────────
# 事实来源：任务看板表格
# ─────────────────────────────────────────────────────────────────────────────
def parse_taskboard():
    rows = []
    for line in (ROOT / "docs/TASKBOARD.md").read_text(encoding="utf-8").splitlines():
        if not line.lstrip().startswith("|"):
            continue
        cells = [c.strip() for c in line.strip().strip("|").split("|")]
        if len(cells) < 6:
            continue
        m = re.fullmatch(r"\*{0,2}(TC-\d{3})\*{0,2}", cells[0])
        if not m:
            continue
        cid = m.group(1)
        sm = re.search(r"\(([^)]+)\)", cells[4])
        spec = sm.group(1) if sm else ""
        if spec.startswith("specs/"):
            spec = "docs/" + spec
        rows.append({
            "id": cid,
            "title": cells[1].strip("*").strip(),
            "status": cells[2].strip("*").strip(),
            "deps": cells[3],
            "spec": spec,
            "adrs": cells[5],
        })
    return rows


TASKS = parse_taskboard()


def scan_spec_status() -> dict[str, str]:
    """读 spec 文件自报的「状态：」字段，仅用于和 TASKBOARD 做漂移核对，不覆盖看板。"""
    out = {}
    for f in sorted((ROOT / "docs/specs").glob("TC-*.md")):
        m = re.search(r"^状态[：:]\s*\*{0,2}([a-z_]+)", f.read_text(encoding="utf-8"), re.M)
        if m:
            out[f.name[:6]] = m.group(1)
    return out


SPEC_SELF_STATUS = scan_spec_status()


def consistency_issues() -> list[str]:
    """源文档之间的漂移：只报告，绝不擅自改任何一份事实来源。"""
    issues = []
    board = {r["id"]: r for r in TASKS}
    for f in sorted((ROOT / "docs/specs").glob("TC-*.md")):
        cid = f.name[:6]
        row = board.get(cid)
        if row and not row["spec"]:
            issues.append(cid + " 的 spec 已落盘（" + f.name + "），但 TASKBOARD 的 spec 列仍是「—」；"
                          "规格覆盖率按看板计，不会把它算进分子")
        st = SPEC_SELF_STATUS.get(cid)
        if row and st and st != row["status"]:
            issues.append(cid + "：spec 自报 <code>" + esc(st) + "</code>，TASKBOARD 记 <code>"
                          + esc(row["status"]) + "</code>；总纲按 TASKBOARD 显示，需人工确认后改看板")
    return issues


ISSUES = consistency_issues()

# 目标 / 验收要点 + 复杂度点数（仅用于加权看板，不是工期承诺）
META: dict[str, tuple[str, int]] = {
    "TC-000": ("工程骨架、深色 Token、9 项门禁、未签名 dmg 链路打通", 3),
    "TC-100": ("加密库打开顺序、版本化迁移、失败回滚转只读；不用连接池", 5),
    "TC-101": ("Keychain put/get/delete；条目不存在是独立错误码", 3),
    "TC-102": ("Argon2id + 失败锁定 + 空闲/唤醒锁定；主线程投递实测", 5),
    "TC-103": ("S01 向导 / S02 解锁；21 条标准中 3 条需真人真机确认", 5),
    "TC-104": ("备份密钥来自用户口令；先校验后原子替换，中途失败不留半库", 5),
    "TC-105": ("清空数据：先删库后删钥匙串条目，连备份一起删、日志留着", 2),
    "TC-200": ("字段配置、停用不删、预览表头是导出表头唯一真相源", 2),
    "TC-201": ("一个邮箱一个人、批量粘贴自动去重、只有停用没有删除", 2),
    "TC-202": ("minijinja 不开 loader、字段变量用 key、空值按必填判定", 3),
    "TC-203": ("自写递归下降求值器；无函数/属性/字符串/I/O，按分四舍五入", 3),
    "TC-300": ("账套状态机；快照在进 ready 时冻结、解锁回 draft 且留痕", 3),
    "TC-301": ("按账套快照导出空 Excel 模板，表头随字段配置动态生成", 2),
    "TC-302": ("导入向导：表头映射 / 行级校验 / 冲突策略 / 错误明细", 5),
    "TC-303": ("逐格编辑 + 公式列自动重算 + 合计行", 3),
    "TC-400": ("令牌桶限速 30 封/分 + 指数退避 30s / 2min / 10min", 3),
    "TC-401": ("三重核对：身份一致 / 内容 checksum / 变量完整", 3),
    "TC-402": ("幂等键 + 队列持久化，强退重启只补未成功的 job", 5),
    "TC-403": ("发送日志脱敏（不含工资数值）+ 凭证 CSV 导出", 2),
    "TC-500": ("lettre + rustls 接真 Sender；StubTransport 验收，真凭据真机验证归 TC-501", 3),
    "TC-501": ("QQ / 163 / 企业邮授权码各 5 封送达验证", 2),
    "TC-502": ("Gmail / M365 OAuth2（PKCE 环回）；可降级为暂不做", 3),
    "TC-600": ("universal 未签名 dmg + SHA-256 + 一页安装说明", 2),
    "TC-601": ("升级演练：覆盖安装后 Keychain 授权重置的处理步骤", 2),
}

STATUS = {
    "accepted": ("✅ 已结案", "bok"),
    "in_progress": ("🔄 进行中", "bing"),
    "blocked": ("🔴 阻塞", "bred"),
    "pending": ("⬜ 未开始", "bno"),
    "review": ("🔍 待评审", "bing"),
    "withdrawn": ("⊘ 已作废", "bno"),
}

PHASES = [
    ("P0", "工程骨架与设计系统", "P0 起点，TC-000 已结案"),
    ("P1", "身份与存储", "仅收口卡 TC-103 剩 3 条真人验收"),
    ("P2", "配置模块", "TC-200~203 全部结案"),
    ("P3", "数据闭环", "TC-300~303 全部结案；账套→空模板→导入→逐格编辑的月度主链路完整"),
    ("P4", "发送引擎（mock SMTP）", "TC-400~403 全部结案；限速退避 / 三重核对 / 幂等恢复 / 凭证留痕的 Rust 侧完成"),
    ("P5", "真实邮箱联调", "TC-500 已发出 spec 与 ADR-0027；真机验证仍等飞书专用密码"),
    ("P6", "发布（未签名内部分发）", "TC-600 已发出 spec 与 ADR-0028；安装说明与升级演练未开始"),
]


def phase_of(cid: str) -> str:
    return "P" + cid[3]


# ─────────────────────────────────────────────────────────────────────────────
# 条目文案：spec 与 ADR 的核心口径（整理自 TASKBOARD「当前焦点」与各 ADR 标题）
# ─────────────────────────────────────────────────────────────────────────────
SPEC_NOTES: dict[str, str] = {
    "TC-000": "P0 起点：Tauri 2 骨架、CSS Modules + 深色 Design Token、统一门禁，以及未签名 dmg 的完整构建链路。",
    "TC-100": "加密数据层：key→参数→读回的顺序、Keychain 取不到 key 绝不新建空库、迁移失败回滚并转只读、关闭即释放连接；明确不用连接池。",
    "TC-101": "秘密存取层：security-framework item 层直连，临时钥匙串上验证 put/get/delete，条目不存在是独立错误码 KEYCHAIN_MISSING。",
    "TC-102": "口令与锁定：argon2 特征裁剪成纯 Rust 依赖、参数写进 PHC 单列、锁定计数进库；唤醒走分布式通知中心，附 §10 四组主线程投递对照实验。",
    "TC-103": "P1 收口卡：S01 首启向导 / S02 解锁界面。21 条标准已跑完 18 条，剩合盖唤醒、空闲自动锁屏、真人确认窗口外观三条必须本人在真机做。",
    "TC-104": "备份的密钥来自用户口令而非钥匙串 DB key（否则新机永远打不开）；SQLCipher 的 ATTACH + sqlcipher_export 加密，恢复走暂存→校验→原子改名。",
    "TC-105": "数据销毁：先删库、后删钥匙串条目（两个半成品状态代价不对称）；连 exports 里的备份一起删，脱敏日志保留，不做不可靠的覆写擦除。",
    "TC-200": "字段是用户数据不是外部输入：停用 = visible=0（不进新表头、不进新模板白名单，但渲染历史模板仍认它）；只有停用没有删除；默认字段集取 S04 原型示例集。",
    "TC-201": "一个邮箱一个人（members.email 全局 UNIQUE，是「三重核对·身份一致」的唯一依据）；批量粘贴自动去重、编辑表单撞了报错；成员只有停用，删组只解归属。",
    "TC-202": "minijinja 但不开 loader ——「从磁盘读模板」这条路在结构上就不存在；字段变量用 key 不用显示名；空值按是否必填判定；试发留到 TC-500。",
    "TC-203": "自己写递归下降算术求值器，语法只有数字 / 字段 key / 汇总变量 / +−×÷() / 一元负号；没有函数、属性、字符串、I/O；求值用原始数值、格式化在后、按分四舍五入。",
    "TC-300": "快照在「进入 ready」那一刻冻结（不是建账套时）；draft 用当前配置、ready 起用快照；is_editable = draft | ready；解锁 sent→draft 且必在 audit_log 留修订记录。",
    "TC-301": "空模板导出：表头 = 3 个固定列（姓名/邮箱/工号）+ 启用字段且固定列在前（导入映射的锚点），取账套快照而非当前配置；公式列只写「公式自动计算」占位；默认空模板（说明 Sheet 开、示例数据与预填名单关），扣款项填负数写进说明 Sheet。",
    "TC-302": "Excel 导入：表头映射分精确 / 模糊 / 未匹配三档，校验分错误 K 阻断 / 警告 M 入库带标记 / 成功 N（判据是「会不会让发出去的钱是错的」）；身份按邮箱找人，姓名不一致只警告并标「身份待核对」且不进发送范围；空金额 = 没有这一项；冲突三选一 + 覆盖前同事务写审计；整批一个事务，原文件只读不复制。",
    "TC-303": "逐格编辑：单格保存触发整行公式列重算与 checksum 重算，返回新行原地替换；公式列前后端两层只读；校验列是必填缺失警告（拦发送不拦保存）；表级合计是整月且公式列不进合计；编辑权在 Command 层再查一次；逐格编辑不写 audit；清空一格 = 删键。",
    "TC-400": "发送编排：令牌桶只判定不等待（时钟注入，测试零 sleep）、降速是批次级且批内只降一次；错误分类是纯函数（535/534 中止整批、4xx 退避重试、5xx 全段判死、无码暂停）；job 状态机没有 failed 存储态；每封完成即持久化；send_jobs 没有 cc/bcc 列也不存正文。",
    "TC-401": "三重核对：身份一致的发送半拦三类（identity_verified=0 / 停用成员 / 校验列有问题）是从范围排除并列进报告，不是报错；变量完整按 required 判定；内容一致用 row_checksum 与 send_jobs.checksum 比对（执行在 TC-402）；预览与核对共用同一份 context_for 渲染。验收补上 TC-302 漏持久化的「身份待核对」标记。",
    "TC-402": "恢复语义：存疑 sending 恢复时回 queued（发给本人的重复无害、丢发难查），已成功绝不重发；恢复可重复发起且进度从库算，没有第二份快照；仅重试失败项（dead→queued、attempts 保留）；点发送两层核对；SMTP 未配置时如实报「发信设置还没配置」而不是假成功。红线 3 与红线 4 在此兑现。",
    "TC-403": "发送凭证：CSV 六列（姓名/收件地址/状态/SMTP回码/发送时间/内容校验和）且逐格扫不到工资数值；姓名进 CSV 不进日志；脱敏四层（库无正文列 / 审计只记行数 / 凭证无数值 / 日志只记 id 与处置）各自被测试钉住；UTF-8 BOM + RFC 4180；send_jobs 没有删除入口，导出是纯读操作。",
    "TC-500": "SMTP 引擎与 S14 邮件设置：lettre + rustls + ring（不引 native-tls，门禁第 9 项禁 OpenSSL）；Sender trait 改收 EmailMessage、批次带 template_id；专用密码进 Keychain；multipart/alternative 配 text/plain 兜底；每封新建会话；测试连接只发一封自检信。全部自动化验收跑 StubTransport，真凭据真机验证归 TC-501。",
    "TC-600": "发布产物与一页安装说明：一条可重复的发布命令（release.sh 跑通并加固）、dmg 随包附一页安装说明（docs/安装说明.md，六节齐全、正文 ≤ 900 字）、一条发布记录（版本 / 体积 / SHA-256 / 架构证据）。未签名的三条真相写进说明：每个新版本手动放行一次、升级后钥匙串授权重置、spctl 预期失败。P6 验收里的「试发一封」需要真实 SMTP 凭据，归 TC-501；升级演练归 TC-601。",
}

SPEC_CRITERIA: dict[str, str] = {
    "TC-100": "11 条", "TC-101": "9 条", "TC-102": "15 条", "TC-103": "21 条（已验 18）",
    "TC-104": "16 条", "TC-200": "12 条", "TC-201": "13 条", "TC-202": "12 条",
    "TC-203": "12 条", "TC-300": "11 条", "TC-301": "12 条", "TC-302": "18 条",
    "TC-303": "13 条", "TC-400": "12 条", "TC-401": "11 条", "TC-402": "10 条",
    "TC-403": "11 条", "TC-500": "9 条（尚未验收）", "TC-600": "12 条（尚未验收）",
}

ADR_NOTES: dict[str, str] = {
    "0001": "技术栈锁定 Tauri 2 + Rust + React/TS（决策 D1），不再评估其他方案。",
    "0002": "不购买 Apple Developer Program：走未签名内部分发。代价是网盘分发需手动放行、每次升级 Keychain 授权会重置。",
    "0003": "把 SDKROOT 钉回 CLT 自带 MacOSX.sdk，绕开混入的 27.0 SDK 导致任何 Rust/C 链接都报 tapi error 的坑。",
    "0004": "错误词表先行、文案由 Rust 侧承载：前端不自行拼错误文案，保证同一错误码在各界面口径一致。",
    "0005": "CSS Modules + Design Token，v1 深色唯一；色值只允许出现在 tokens.css，例外须写 no-token 注释供 grep 审计。",
    "0006": "npm 镜像源只作为本机下载加速参数，不写进仓库配置，避免把开发机网络环境固化进构建。",
    "0007": "Tauri 版本对齐与 MSRV 策略：CLI / API / 插件版本成组对齐，避免脚手架与运行时漂移。",
    "0008": "dmg 由 hdiutil 产出，不依赖 Finder —— 验收常在后台 shell 里跑，GUI 会话不可靠。",
    "0009": "Keychain 用 security-framework 的 item 层直连 + 临时钥匙串做测试；补充章节记录开发循环反复弹授权窗的根因（cdhash / Apple 锚定签名）与 dev-run.sh 解法。",
    "0010": "SQLCipher 用 rusqlite bundled 静态编译走 CommonCrypto；参数在 key 之后设、不用池、迁移记录进库。探针 10 把读回守卫收窄到 page_size + kdf_iter 两项。",
    "0011": "管理员口令：argon2 关默认特征减依赖面、参数钉 OWASP 2024 下限并写进 PHC 串、锁定计数进库、唤醒走分布式通知中心。§10 四组对照证明回调只由主线程 runloop 派发。",
    "0012": "首启与解锁的十条界面口径：数据目录跟 app_data_dir() 走、SMTP 测试连接后置到 TC-500、迁移漂移专属码 MIGRATION_DRIFT、只读态常驻横幅、「记住本机 10 分钟」的定义、忘记密码不给假按钮等。",
    "0013": "备份密钥来自用户口令、SQLCipher 换密钥导出、先校验后替换六步顺序。实现期追加记录两个真 bug：临时目录名撞秒、panic 时清理被跳过导致工资数据留盘。",
    "0014": "清空数据的删除顺序：先删库再删钥匙串条目，连备份一起删、日志留着；真正的安全性质是「密钥没了 ⇒ 密文永远解不开」。",
    "0015": "工资条项目：停用即不进新表头、没有删除操作、默认字段集用设计文档 S04 的示例集；停用与历史渲染是两份不同的名单。",
    "0016": "成员名单：一个邮箱一个人、批量粘贴自动去重（表单则报错）、只有停用没有删除；「邮箱待确认」第三态归 TC-403。",
    "0017": "模板渲染用 minijinja 但不开 loader，配变量白名单，字段变量用 key 不用显示名 —— 沙箱靠结构而不是靠约定。",
    "0018": "公式求值器自己写递归下降解析器而不用模板引擎；求值用原始数值、格式化在求值之后，除零 / 循环引用 / 文本做算术一律报错。",
    "0019": "月份账套：快照在进入 ready 时冻结、ready 仍可编辑（修正 TC-000 时代的口径）、解锁回 draft 且必定留修订记录。",
    "0020": "空模板导出：表头 = 3 固定列 + 启用字段且固定列在前（导入映射的锚点）、表头取账套快照、公式列只写占位文字、默认是空模板；验收修了 Sheet 命名、落盘路径分散与 clippy 三处告警。",
    "0021": "Excel 导入：表头映射三档、校验三档（判据是「会不会让发出去的钱是错的」）、身份按邮箱、姓名不一致只警告且不进发送范围、空金额 = 没有这一项、冲突三选一且覆盖前同事务写审计、整批一个事务、原文件只读不复制。",
    "0022": "逐格编辑：单格保存 → 整行公式重算 → 新行原地替换、公式列两层只读、校验列拦发送不拦保存、表级合计不含公式列、逐格编辑不写 audit（避免 420 条把审计日志淹没）。",
    "0023": "发送编排：令牌桶只判定不等待（时钟注入）、降速批次级且批内只降一次、错误分类是纯函数、job 无 failed 存储态、每封完成即持久化、send_jobs 无 cc/bcc 与正文列。验收把未知 5xx（531）从 Retry 改成全段判死。",
    "0024": "三重核对：身份一致的发送半拦三类是排除并列进报告而非报错、变量完整按 required、内容一致用 checksum 比对（执行在 TC-402）、预览与核对共用同一份渲染；验收补上 TC-302 漏持久化的 identity_verified（迁移 0009）。",
    "0025": "恢复语义：存疑 sending 回 queued（重复无害、丢发难查）、已成功绝不重发、进度从库算没有第二份快照、仅重试失败项、点发送两层核对、SMTP 未配置时如实报错；红线 3（每封一个收件人，绝不抄送）与红线 4（只补未成功）在此兑现。",
    "0026": "发送凭证：CSV 六列且无工资数值、姓名进 CSV 不进日志、脱敏四层各有测试、UTF-8 BOM + RFC 4180、send_jobs 没有删除入口（≥2 年留存靠「不删」，唯一终点是 TC-105）、导出是纯读。",
    "0027": "SMTP 引擎：lettre + rustls + ring 避开 OpenSSL（门禁第 9 项）；Sender trait 收 EmailMessage 让渲染留在编排、渲染失败只判该封 dead 而批次继续；批次带 template_id；专用密码进 Keychain；multipart/alternative；每封新建会话；测试连接 = 自检信，自动化验收全跑 StubTransport。",
    "0028": "未签名内部分发的口径：v1 不签名不公证（D8 延续，sign/notarize 留着但默认不跑）；分发判据是 quarantine 属性而不是介质名称（U 盘与局域网共享拷贝不带 → 双击直开，网盘下载带 → 每台机首次手动放行）；钥匙串弹窗必须点「允许」（未签名 ACL 认 cdhash，点「拒绝」会锁在门外）；spctl 预期失败是事实不是缺陷；产物不入 git；升级演练归 TC-601。",
    "0029": "重新初始化：读不出来的旧库改名移走（不删、不碰钥匙串、不碰备份），错误页给出重试/从备份恢复/重新初始化三条真能点的路 —— 文案承诺的每条路都必须有按钮顶着。",
    "0030": "开发构建与发布构建分数据目录（dev 用 -dev 后缀）：两边钥匙串本来就不同却共用一个 data.db，必然撞「密钥与数据不匹配」；分流是编译期的，发布二进制里没有 dev 路径。",
}


# ─────────────────────────────────────────────────────────────────────────────
# 静态骨架：样式
# ─────────────────────────────────────────────────────────────────────────────
CSS = r"""
:root{
  --bg:#0e1320; --panel:#161d2e; --panel-2:#1b2438; --ink:#e8eefb; --ink-2:#b3c0d6; --ink-3:#8493ad;
  --line:#2a3549; --bar:#222d42;
  --accent:#6ea8ff; --accent-soft:#16294a; --accent-line:#2e4e80;
  --green:#4ade9b; --green-soft:#123528;
  --amber:#f5c26b; --amber-soft:#3a2c12;
  --red:#ff8f8f; --red-soft:#3b1f24;
  --purple:#c4a5ff; --purple-soft:#2a2042;
}
html[data-theme="light"]{
  --bg:#f8fafc; --panel:#ffffff; --panel-2:#fbfcfe; --ink:#1e293b; --ink-2:#475569; --ink-3:#64748b;
  --line:#e2e8f0; --bar:#eef2f7;
  --accent:#2563eb; --accent-soft:#eff6ff; --accent-line:#bfdbfe;
  --green:#059669; --green-soft:#ecfdf5;
  --amber:#b45309; --amber-soft:#fffbeb;
  --red:#dc2626; --red-soft:#fef2f2;
  --purple:#7c3aed; --purple-soft:#f5f3ff;
}
*{box-sizing:border-box;}
body{margin:0;background:var(--bg);color:var(--ink);
  font:15px/1.75 -apple-system,BlinkMacSystemFont,"PingFang SC","Microsoft YaHei","Segoe UI",sans-serif;}
a{color:var(--accent);text-decoration:none;} a:hover{text-decoration:underline;}
.wrap{max-width:1060px;margin:0 auto;padding:30px 22px 80px;}
header.hero{position:relative;background:linear-gradient(135deg,var(--panel) 55%,var(--accent-soft) 100%);
  border:1px solid var(--line);border-radius:16px;padding:24px 30px;margin-bottom:16px;}
h1{margin:0 0 6px;font-size:24px;line-height:1.4;}
.meta{color:var(--ink-3);font-size:13px;margin-bottom:12px;} .meta span{margin-right:14px;}
.oneliner{background:var(--panel-2);border:1px solid var(--line);border-radius:10px;padding:10px 15px;font-size:13.5px;margin-top:9px;}
.tools{position:absolute;right:24px;top:22px;display:flex;gap:7px;flex-wrap:wrap;}
.tools button{border:1px solid var(--line);background:var(--panel-2);color:var(--ink-2);border-radius:8px;
  padding:5px 11px;font-size:12px;cursor:pointer;}
.tools button:hover{border-color:var(--accent);color:var(--accent);}
/* 看板 */
.board{background:var(--panel);border:1px solid var(--line);border-radius:16px;padding:20px 26px;margin-bottom:16px;}
.board-title{font-weight:700;font-size:15px;margin-bottom:14px;display:flex;align-items:center;gap:8px;}
.board-title .dot{width:9px;height:9px;border-radius:50%;background:var(--accent);}
.overall-progress{margin:0 0 18px;padding:14px;background:var(--panel-2);border:1px solid var(--line);border-radius:12px}
.overall-progress h2{margin:0 0 10px;font-size:15px}
.progress-grid{display:grid;grid-template-columns:repeat(auto-fit,minmax(min(100%,195px),1fr));gap:10px}
.progress-metric{min-width:0;padding:10px 13px;background:var(--panel);border:1px solid var(--line);border-radius:9px}
.progress-metric span{font-size:12.5px;color:var(--ink-2)}
.progress-metric strong{display:block;font-size:25px;line-height:1.3;color:var(--accent)}
.progress-metric small,.overall-progress details{color:var(--ink-2);font-size:12px}
.progress-metric progress{display:block;width:100%;height:8px;margin:6px 0;accent-color:var(--accent)}
.overall-progress details{margin-top:10px} .overall-progress summary{cursor:pointer;font-weight:600}
.overall-progress details p{margin:6px 0 0}
.rail{display:flex;align-items:flex-start;position:relative;margin:6px 0 4px;}
.rail::before{content:"";position:absolute;left:16px;right:16px;top:9px;height:3px;background:var(--line);border-radius:2px;}
.node{flex:1;position:relative;z-index:1;display:flex;flex-direction:column;align-items:center;gap:6px;text-align:center;}
.node .b{width:19px;height:19px;border-radius:50%;background:var(--panel);border:3px solid var(--line);}
.node.done .b{background:var(--green);border-color:var(--green);}
.node.doing .b{border-color:var(--accent);background:var(--panel);box-shadow:0 0 0 4px var(--accent-soft);}
.node.blocked .b{border-color:var(--red);background:var(--red-soft);}
.node .nl{font-size:11.5px;color:var(--ink-2);line-height:1.4;max-width:118px;}
.node.done .nl{color:var(--green);font-weight:600;} .node.doing .nl{color:var(--accent);font-weight:600;}
.node.blocked .nl{color:var(--red);font-weight:600;}
.node .nd{font-size:10.5px;color:var(--ink-3);}
.rail-cap{font-size:12px;color:var(--ink-3);margin-top:10px;text-align:center;} .rail-cap b{color:var(--accent);}
/* 分期任务表 */
.devboard{margin-top:12px;border:1px solid var(--line);border-radius:12px;padding:12px 16px;background:var(--panel-2)}
.devboard .dbt{font-weight:700;font-size:13.5px;margin-bottom:8px;display:flex;gap:10px;align-items:center;flex-wrap:wrap}
.devboard .dbt small{font-weight:400;font-size:11.5px;color:var(--ink-3)}
.devboard .tabs{display:flex;gap:6px;flex-wrap:wrap;margin-bottom:10px}
.devboard .tab{border:1px solid var(--line);background:var(--panel);border-radius:999px;padding:4px 13px;
  font-size:12.5px;cursor:pointer;color:var(--ink-2)}
.devboard .tab.active{background:var(--accent);color:#fff;border-color:var(--accent);font-weight:600}
.devboard .sum{font-size:12.5px;color:var(--ink-2);margin-bottom:8px}
.devboard .pbar{height:9px;background:var(--bar);border-radius:99px;overflow:hidden;margin-bottom:10px}
.devboard .pbar i{display:block;height:100%;background:linear-gradient(90deg,var(--green),#7ce6bb)}
.devboard table{width:100%;border-collapse:collapse;font-size:12.5px}
.devboard th{background:var(--bar);padding:6px 8px;border:1px solid var(--line);font-weight:600;color:var(--ink-2);text-align:left}
.devboard td{padding:6px 8px;border:1px solid var(--line);vertical-align:top}
.devboard .id{font-weight:700;white-space:nowrap}
.devboard .g{font-weight:600;min-width:130px}
.devboard .bok,.devboard .bing,.devboard .bno,.devboard .bred{border-radius:999px;padding:1px 9px;font-size:11.5px;white-space:nowrap}
.devboard .bok{background:var(--green-soft);color:var(--green);}
.devboard .bing{background:var(--amber-soft);color:var(--amber);}
.devboard .bno{background:var(--bar);color:var(--ink-3);}
.devboard .bred{background:var(--red-soft);color:var(--red);}
.devboard .dbnote{font-size:11.5px;color:var(--ink-3);margin-top:8px}
/* 统计与分类填充 */
.stats{display:grid;grid-template-columns:repeat(auto-fit,minmax(128px,1fr));gap:10px;margin:16px 0 4px;}
.stat{background:var(--panel-2);border:1px solid var(--line);border-radius:11px;padding:10px 8px;text-align:center;}
.stat b{display:block;font-size:21px;color:var(--accent);} .stat span{font-size:11.5px;color:var(--ink-3);}
.fill{margin-top:12px;display:flex;flex-direction:column;gap:7px;}
.frow{display:flex;align-items:center;gap:10px;font-size:12.5px;}
.frow .fn{flex:0 0 175px;color:var(--ink-2);white-space:nowrap;overflow:hidden;text-overflow:ellipsis;}
.frow .bar{flex:1;height:9px;background:var(--bar);border-radius:99px;overflow:hidden;}
.frow .bar i{display:block;height:100%;background:var(--accent);border-radius:99px;transition:width .4s;}
.frow .fv{flex:0 0 100px;color:var(--ink-3);text-align:right;font-size:11.5px;}
.chipline{display:flex;gap:8px;flex-wrap:wrap;margin-top:12px;}
.kchip{border-radius:999px;padding:3px 12px;font-size:12px;background:var(--amber-soft);color:var(--amber);border:1px solid var(--line);}
.kchip.g{background:var(--green-soft);color:var(--green);} .kchip.r{background:var(--red-soft);color:var(--red);}
/* 检索 */
.gsearch{background:var(--panel);border:1px solid var(--line);border-radius:16px;padding:16px 26px;margin-bottom:16px;}
.gsearch .row{display:flex;gap:10px;align-items:center;}
#gq{flex:1;padding:10px 14px;border:1px solid var(--line);border-radius:10px;font-size:14.5px;outline:none;
  background:var(--panel-2);color:var(--ink);}
#gq:focus{border-color:var(--accent);box-shadow:0 0 0 3px var(--accent-soft);}
#gclear{border:1px solid var(--line);background:var(--panel-2);border-radius:10px;padding:9px 16px;
  font-size:13px;color:var(--ink-2);cursor:pointer;}
#gclear:hover{border-color:var(--accent);color:var(--accent);}
#ghint{font-size:12.5px;color:var(--ink-3);margin-top:8px;}
/* 分类 */
section.cat{background:var(--panel);border:1px solid var(--line);border-radius:16px;margin-bottom:14px;overflow:hidden;}
.sec-head{display:flex;align-items:center;gap:10px;padding:13px 20px;cursor:pointer;user-select:none;background:var(--panel-2);}
.sec-head:hover{background:var(--accent-soft);}
.sec-head .tgl{width:22px;height:22px;border-radius:6px;border:1px solid var(--line);background:var(--panel);
  color:var(--ink-2);font-size:11px;line-height:1;cursor:pointer;flex:0 0 auto;}
.sec-head .no{flex:0 0 auto;min-width:30px;height:26px;border-radius:8px;background:var(--accent);color:#fff;
  font-size:12.5px;display:inline-flex;align-items:center;justify-content:center;padding:0 8px;font-weight:600;}
.sec-head h2{margin:0;font-size:16px;flex:0 0 auto;}
.sec-head .cnt{flex:0 0 auto;font-size:12px;color:var(--ink-3);background:var(--bar);border-radius:999px;padding:2px 10px;}
.sec-head .stchip{flex:0 0 auto;font-size:11.5px;border-radius:999px;padding:2px 10px;}
.st-ok{background:var(--green-soft);color:var(--green);} .st-wait{background:var(--amber-soft);color:var(--amber);}
.st-red{background:var(--red-soft);color:var(--red);} .st-meta{background:var(--purple-soft);color:var(--purple);}
.sec-head .ssearch{flex:1;min-width:120px;max-width:240px;margin-left:auto;padding:6px 11px;border:1px solid var(--line);
  border-radius:8px;font-size:12.5px;outline:none;background:var(--panel);color:var(--ink);}
.sec-head .ssearch:focus{border-color:var(--accent);}
.sec-body{padding:14px 20px 16px;} section.cat.collapsed .sec-body{display:none;}
.file{display:flex;gap:14px;align-items:flex-start;padding:13px 14px;border:1px solid var(--line);
  border-radius:11px;margin-bottom:10px;background:var(--panel-2);transition:border-color .15s;}
.file:hover{border-color:var(--accent-line);}
.badge{flex:0 0 auto;margin-top:3px;font-size:12px;font-weight:600;border-radius:7px;padding:2px 9px;white-space:nowrap;}
.b-doc{background:var(--accent-soft);color:var(--accent);} .b-rpt{background:var(--purple-soft);color:var(--purple);}
.b-plan{background:var(--green-soft);color:var(--green);} .b-data{background:var(--amber-soft);color:var(--amber);}
.b-other{background:var(--bar);color:var(--ink-2);} .b-red{background:var(--red-soft);color:var(--red);}
.fbody{flex:1;min-width:0;} .fname{font-size:15px;font-weight:600;}
.fname .sub{font-size:12px;font-weight:400;}
.fdesc{font-size:13.5px;color:var(--ink-2);margin-top:2px;}
.fmeta{font-size:12.5px;color:var(--ink-3);margin-top:4px;}
.nomatch{display:none;border:1px dashed var(--line);border-radius:11px;padding:12px;color:var(--amber);font-size:13px;text-align:center;}
table{width:100%;border-collapse:collapse;margin:10px 0;font-size:13.5px;}
th{background:var(--bar);text-align:left;padding:8px 10px;border:1px solid var(--line);font-weight:600;color:var(--ink-2);white-space:nowrap;}
td{padding:8px 10px;border:1px solid var(--line);vertical-align:top;}
tr:nth-child(even) td{background:var(--panel-2);}
.callout{border-radius:10px;padding:12px 16px;margin:12px 0;font-size:13.5px;}
.callout b.lbl{display:block;margin-bottom:4px;}
.c-blue{background:var(--accent-soft);border:1px solid var(--accent-line);}
.c-green{background:var(--green-soft);border:1px solid var(--line);}
.c-amber{background:var(--amber-soft);border:1px solid var(--line);}
code{background:var(--bar);border-radius:5px;padding:1px 6px;font-size:12.5px;color:var(--ink);}
ol,ul{margin:8px 0;padding-left:22px;} li{margin:3px 0;}
footer{color:var(--ink-3);font-size:12.5px;text-align:center;margin-top:22px;}
/* 抽屉：最新更新 */
#dw-fab{position:fixed;right:22px;bottom:22px;z-index:60;background:var(--accent);color:#fff;border:none;
  border-radius:99px;padding:10px 18px;font-size:13.5px;font-weight:600;box-shadow:0 6px 18px rgba(0,0,0,.28);cursor:pointer;}
#dw-fab:hover{opacity:.92;}
#dw-mask{display:none;position:fixed;inset:0;background:rgba(0,0,0,.45);z-index:70;} #dw-mask.on{display:block;}
#upd-drawer{position:fixed;top:0;right:-400px;width:380px;max-width:92vw;height:100vh;background:var(--panel);
  z-index:71;box-shadow:-8px 0 30px rgba(0,0,0,.3);transition:right .22s ease;display:flex;flex-direction:column;}
#upd-drawer.on{right:0;}
.dw-head{display:flex;align-items:center;justify-content:space-between;padding:16px 20px;border-bottom:1px solid var(--line);}
.dw-head button{border:none;background:var(--bar);border-radius:8px;width:28px;height:28px;cursor:pointer;color:var(--ink-2);}
.dw-tip{padding:10px 20px;font-size:12px;color:var(--ink-3);border-bottom:1px solid var(--line);background:var(--accent-soft);}
#drawer-list{overflow-y:auto;flex:1;padding:8px 0;}
.dw-item{display:flex;gap:12px;padding:10px 20px;border-bottom:1px dashed var(--line);align-items:flex-start;}
.dw-no{flex-shrink:0;width:22px;height:22px;border-radius:50%;background:var(--accent-soft);color:var(--accent);
  font-size:12px;font-weight:700;display:flex;align-items:center;justify-content:center;margin-top:2px;}
.dw-body{min-width:0;} .dw-body a{font-size:13px;} .dw-date{font-size:11.5px;color:var(--ink-3);}
@media(max-width:680px){
  .tools{position:static;margin-bottom:12px;} .sec-head{flex-wrap:wrap;}
  .sec-head .ssearch{max-width:none;width:100%;order:9;} .file{flex-direction:column;gap:6px;}
  .wrap{padding:18px 12px 60px;} .rail .nl{font-size:10px;} .frow .fn{flex-basis:120px;}
}
@media print{ .gsearch,.sec-head .ssearch,.sec-head .tgl,.tools,#dw-fab,#upd-drawer,#dw-mask{display:none!important;}
  .wrap{padding:0;} section.cat.collapsed .sec-body{display:block!important;} }
"""

# ─────────────────────────────────────────────────────────────────────────────
# 静态骨架：交互脚本
# ─────────────────────────────────────────────────────────────────────────────
JS = r"""
<script>
(function(){
  var sections=[].slice.call(document.querySelectorAll('section.cat'));
  var gq=document.getElementById('gq'), ghint=document.getElementById('ghint');

  function entryText(f){ return ((f.dataset.keywords||'')+' '+f.textContent).toLowerCase(); }
  function setCollapsed(sec,c){
    sec.classList.toggle('collapsed',c);
    var t=sec.querySelector('.tgl'); if(t)t.textContent=c?'▸':'▾';
  }
  function filterSection(sec, kw, forceOpen){
    var files=[].slice.call(sec.querySelectorAll('.file'));
    var nomatch=sec.querySelector('.nomatch'), hits=0;
    files.forEach(function(f){
      var ok=!kw||entryText(f).indexOf(kw)>-1;
      f.style.display=ok?'':'none'; if(ok)hits++;
    });
    /* 07 / ⚙ 里的红线、决策、遗留表格不是登记条目：检索时收起，避免「无匹配」却仍铺一屏表格 */
    var body=sec.querySelector('.sec-body');
    if(body)[].slice.call(body.children).forEach(function(ch){
      if(ch.classList.contains('file')||ch.classList.contains('nomatch'))return;
      ch.style.display=kw?'none':'';
    });
    if(kw){
      setCollapsed(sec,false);
      if(nomatch)nomatch.style.display=(hits===0)?'block':'none';
    }else{
      if(nomatch)nomatch.style.display='none';
      if(!forceOpen)setCollapsed(sec,true);
    }
    return hits;
  }
  /* 分类计数与填充条 */
  var counts=[],maxE=1;
  sections.forEach(function(sec){
    if(sec.id==='cat-meta')return;
    var n=sec.querySelectorAll('.file').length;
    var c=sec.querySelector('.cnt'); if(c)c.textContent=n+' 条目';
    counts.push({sec:sec,entries:n}); if(n>maxE)maxE=n;
    var f=sec.querySelector('.fill-note'); if(f)f.textContent=n;
  });
  var tE=0,active=0,uniq={};
  counts.forEach(function(c){tE+=c.entries;if(c.entries>0)active++;});
  document.querySelectorAll('.fname a').forEach(function(x){
    uniq[x.getAttribute('href').split('#')[0]]=1;
  });
  document.getElementById('stFiles').textContent=Object.keys(uniq).length;
  document.getElementById('stEntries').textContent=tE;
  document.getElementById('stCats').textContent=active+' / '+counts.length;
  var fill=document.getElementById('fill');
  counts.forEach(function(c){
    var name=c.sec.querySelector('h2').textContent;
    var row=document.createElement('div');row.className='frow';
    row.innerHTML='<span class="fn">'+(c.entries>0?'✅':'🕐')+' '+c.sec.querySelector('.no').textContent+' · '+escHtml(name)+'</span>'+
      '<span class="bar"><i style="width:'+(c.entries/maxE*100)+'%"></i></span>'+
      '<span class="fv">'+c.entries+' 条目</span>';
    fill.appendChild(row);
  });
  function escHtml(s){return s.replace(/[&<>]/g,function(m){return {'&':'&amp;','<':'&lt;','>':'&gt;'}[m];});}
  /* 折叠 / 展开 */
  sections.forEach(function(sec){
    var head=sec.querySelector('.sec-head');
    head.addEventListener('click',function(e){
      if(e.target.tagName==='INPUT'||e.target.tagName==='A'||e.target.tagName==='BUTTON')return;
      setCollapsed(sec,!sec.classList.contains('collapsed'));
    });
    var inp=sec.querySelector('.ssearch');
    if(!inp)return;
    inp.addEventListener('input',function(e){e.stopPropagation();filterSection(sec,inp.value.trim().toLowerCase(),true);});
    inp.addEventListener('click',function(e){e.stopPropagation();});
  });
  document.getElementById('expand-all').onclick=function(){sections.forEach(function(s){setCollapsed(s,false);});};
  document.getElementById('collapse-all').onclick=function(){sections.forEach(function(s){setCollapsed(s,true);});};
  /* 全文检索 */
  function globalFilter(){
    var kw=(gq.value||'').trim().toLowerCase(),hits=0;
    sections.forEach(function(sec){hits+=filterSection(sec,kw,!!kw);});
    ghint.textContent=kw?('全文检索：命中 '+hits+' 个条目，命中的分类已自动展开。')
      :'输入即跨分类过滤并自动展开命中分类；清空恢复默认视图（全部分类折叠）。';
  }
  gq.addEventListener('input',globalFilter);
  document.getElementById('gclear').onclick=function(){
    gq.value='';globalFilter();
    sections.forEach(function(sec){var i=sec.querySelector('.ssearch');if(i)i.value='';});
  };
  /* 主题 */
  var tb=document.getElementById('theme-btn');
  try{ if(localStorage.getItem('payslip-master-theme')==='light')document.documentElement.dataset.theme='light'; }catch(e){}
  function syncTheme(){ tb.textContent=document.documentElement.dataset.theme==='light'?'🌙 深色':'☀ 浅色'; }
  syncTheme();
  tb.onclick=function(){
    var light=document.documentElement.dataset.theme!=='light';
    if(light)document.documentElement.dataset.theme='light';else delete document.documentElement.dataset.theme;
    try{localStorage.setItem('payslip-master-theme',light?'light':'dark');}catch(e){}
    syncTheme();
  };
})();

/* 分期任务表：状态来自 TASKBOARD，进度由 DOM 实时计算，避免手改漏更 */
(function(){
  var panels=[].slice.call(document.querySelectorAll('.devboard .panel'));
  panels.forEach(function(panel){
    var rows=[].slice.call(panel.querySelectorAll('table tr[data-id]'));
    var ok=0,ing=0,no=0,blk=0;
    rows.forEach(function(r){
      if(r.querySelector('.bok'))ok++;
      else if(r.querySelector('.bing'))ing++;
      else if(r.querySelector('.bred'))blk++;
      else no++;
    });
    var total=rows.length,pct=total?Math.round(ok/total*100):0;
    var sum=panel.querySelector('.sum');
    if(sum)sum.innerHTML='✅ 已结案 '+ok+' · 🔄 进行中 '+ing+' · 🔴 阻塞 '+blk+' · ⬜ 未开始 '+no+
      '　<b style="color:var(--green)">'+ok+'/'+total+'（'+pct+'%）</b>';
    var bar=panel.querySelector('.pbar i'); if(bar)bar.style.width=pct+'%';
  });
  var cards=[].slice.call(document.querySelectorAll('.devboard .panel table tr[data-id]')).map(function(r){
    return {id:r.dataset.id,title:r.querySelector('.g').textContent.trim(),
      done:!!r.querySelector('.bok'),hasspec:r.dataset.hasspec==='1',
      points:Number(r.dataset.complexity)||0};
  });
  function metric(id,done,total,note){
    var el=document.getElementById(id); if(!el)return;
    var pct=total?done/total*100:0;
    el.querySelector('strong').textContent=total?pct.toFixed(1)+'%':'待统计';
    el.querySelector('small').textContent=note||done+' / '+total;
    el.querySelector('progress').value=pct;
  }
  metric('task-progress',cards.filter(function(c){return c.done;}).length,cards.length,'按整卡 accepted 计，含未开工与阻塞卡');
  metric('spec-progress',cards.filter(function(c){return c.hasspec;}).length,cards.length,'已发出 spec 的卡 / 全部卡');
  var pt=cards.reduce(function(s,c){return s+c.points;},0);
  var pd=cards.reduce(function(s,c){return s+(c.done?c.points:0);},0);
  metric('weighted-progress',pd,pt,'按任务范围估点数，不是工期承诺');
  var heavy=cards.filter(function(c){return !c.done&&c.points===5;});
  var rh=document.getElementById('remaining-heavy');
  if(rh)rh.textContent=heavy.length?('未完成的 5 点卡（'+heavy.length+' 张）：'+heavy.map(function(c){return c.id+' '+c.title;}).join('、')+'。'):'当前无未完成的 5 点卡。';
  var tabs=[].slice.call(document.querySelectorAll('.devboard .tab'));
  tabs.forEach(function(t){
    t.onclick=function(){
      tabs.forEach(function(x){x.classList.remove('active');});
      t.classList.add('active');
      document.querySelectorAll('.devboard .panel').forEach(function(p){p.hidden=(p.id!==t.dataset.p);});
    };
  });
})();

/* 最新更新抽屉：从条目 data-date 动态提取，登记即纳入 */
document.addEventListener('DOMContentLoaded',function(){
  var items=[];
  document.querySelectorAll('.file').forEach(function(f){
    var a=f.querySelector('.fname a'); if(!a)return;
    items.push({date:f.dataset.date||'0000-00-00',title:f.dataset.title||a.textContent.trim(),href:a.getAttribute('href')});
  });
  items.sort(function(x,y){return y.date.localeCompare(x.date);});
  document.getElementById('drawer-list').innerHTML=items.slice(0,12).map(function(it,i){
    return '<div class="dw-item"><span class="dw-no">'+(i+1)+'</span><div class="dw-body">'+
      '<a href="'+it.href+'">'+it.title.replace(/[&<>]/g,function(m){return {'&':'&amp;','<':'&lt;','>':'&gt;'}[m];})+'</a>'+
      '<div class="dw-date">'+(it.date==='0000-00-00'?'（未标日期）':it.date)+'</div></div></div>';
  }).join('');
  var d=document.getElementById('upd-drawer'),m=document.getElementById('dw-mask');
  document.getElementById('dw-fab').onclick=function(){d.classList.add('on');m.classList.add('on');};
  document.getElementById('dw-close').onclick=function(){d.classList.remove('on');m.classList.remove('on');};
  m.onclick=function(){d.classList.remove('on');m.classList.remove('on');};
});
</script>
"""


# ─────────────────────────────────────────────────────────────────────────────
# 条目构造
# ─────────────────────────────────────────────────────────────────────────────
def E(badge: str, bcls: str, name: str, desc: str, meta: str, kw: str, date: str, title: str = "") -> str:
    draw = ' data-title="' + esc(title) + '"' if title else ""
    return (
        '<div class="file" data-keywords="' + esc(kw) + '" data-date="' + esc(date) + '"' + draw + ">"
        + '<span class="badge ' + bcls + '">' + esc(badge) + "</span>"
        + '<div class="fbody"><div class="fname">' + name + "</div>"
        + '<div class="fdesc">' + desc + "</div>"
        + '<div class="fmeta">' + meta + "</div></div></div>"
    )


def tb_link(rel: str) -> str:
    return a(rel, "打开源文档", cls="sub")


def build_categories() -> list[tuple[str, str, str, str, str, str]]:
    """返回 [(编号, 名称, id, 状态chip类, chip文字, sec-body HTML), ...]"""
    cats: list[tuple[str, str, str, str, str, str]] = []

    # ── 01 项目宪法与入口 ────────────────────────────────────────────────
    p = []
    p.append(E("总入口", "b-doc", a("总纲.html", "总纲.html · 本页（当前文件）"),
               "项目唯一入口：上半部分是进度与分期任务看板，下半部分按分类登记全部产出；支持跨分类全文检索、分类内检索、折叠、明暗切换。",
               VERSION + " · " + BUILT + " 建立 · " + UPDATED + " 更新 · 由 docs/build-master-index.py 生成，状态取自 TASKBOARD", "总纲 入口 索引 看板 检索 维护规范 master index", UPDATED))
    p.append(E("项目说明", "b-doc", a("README.md", "README.md · 仓库地图与快速开始"),
               "一句话目标、仓库结构与总纲入口、快速开始、P0–P6 进度表、已确认决策摘要、8 条开发红线；新人第一份该读的文件。",
               "2026-11-11 建立 · 2026-11-12 同步进度表与 TASKBOARD、接入总纲入口 · 完整决策 D1–D8 见 PREP-CHECKLIST", "README 仓库结构 快速开始 进度 决策 红线 入口", "2026-11-12"))
    p.append(E("开发前准备", "b-plan", a("PREP-CHECKLIST.md", "PREP-CHECKLIST.md · 开发前准备清单"),
               "D1–D8 八条已拍板决策及其对开发的约束、本机环境实测、🔴 业务输入清单、P0–P6 分阶段验收标准、红线自查、未解决事项。",
               "基线：设计文档 v1.0（2026-11-09）+ 2026-11-11 决策补充 · D8 取消签名公证依赖", "决策 D1 D8 环境 业务输入 验收 飞书 未签名 分发", "2026-11-11"))
    p.append(E("开发约定", "b-plan", a("docs/README.md", "docs/README.md · 开发工作约定"),
               "spec / ADR / TASKBOARD 三种文档的分工、任务卡编号规则、一张卡必须有的字段、验收怎么做、统一门禁，以及每张卡默认包含的红线。",
               "2026-11-11 建立 · 2026-11-12 补 §0「总纲与事实来源边界」· 一句话区分：spec 会随实现重写，ADR 只追加不修改", "约定 spec ADR 任务卡 编号 验收 门禁 事实来源", "2026-11-12"))
    p.append('<div class="callout c-blue"><b class="lbl">事实来源边界（本总纲的定位）</b>'
             '本项目沿用的是「窄事实来源」：<code>docs/</code> 是唯一事实来源 —— 决策看 ADR、做成什么样看 spec、'
             '当前状态看 TASKBOARD。总纲只是<b>导航与看板</b>，不产生新决策、不改写验收标准；'
             '若本页任何数字或状态与源文档不一致，<b>以源文档为准</b>并重跑生成器。'
             '这与参考项目「总纲即唯一入口」的差别是刻意的：本项目的卡要逐条按 spec 验收，容不下第二份状态副本。</div>')
    cats.append(("01", "项目宪法与入口", "cat-01", "st-ok", "✅ 已建成", "".join(p)))

    # ── 02 设计与界面原型 ────────────────────────────────────────────────
    p = []
    design = "payslip-mailer-design.html"
    p.append(E("设计全集", "b-doc", a(design, "payslip-mailer-design.html · 完整设计方案与 UI 原型全集"),
               "15 章方案 + 15 张高保真 macOS 深色界面原型（S01–S15），单文件自包含，浏览器直接打开。"
               "覆盖产品定位、技术选型、系统架构、防错机制、数据模型、发送引擎、模板与 Excel、安全合规、UI 设计系统、路线与验收、风险。",
               "v1.0 · 2026-11-09 建立，同日增补第 15 章「决策落地、本机环境与附件策略」· 约 159 KB", "设计 方案 原型 UI 15章 S01 S15 架构 数据模型 防错", "2026-11-09"))
    chapters = [("p1", "执行摘要"), ("p2", "产品定位与需求映射"), ("p3", "技术选型"), ("p4", "系统架构与工程结构"),
                ("p5", "功能模块与防错机制"), ("p6", "数据设计与表定义"), ("p7", "邮件发送引擎"), ("p8", "模板 / Excel / 变量"),
                ("p9", "安全与合规"), ("p10", "打包签名公证分发"), ("p11", "UI 设计系统与色彩 Token"),
                ("p12", "界面原型全集"), ("p13", "开发路线与验收标准"), ("p14", "风险与待确认"), ("p15", "决策落地与环境")]
    subs = " · ".join(a(design, "ch." + str(i + 1), anc, "sub") for i, (anc, _) in enumerate(chapters))
    p.append(E("章节速查", "b-other", '<span>15 章方案锚点速查</span><span class="sub">　' + subs + "</span>",
               "按章直达：数据设计 (#p6) 是 TC-100 / TC-300 的表结构依据；第 7 章给邮箱配置矩阵与错误码处置；"
               "第 11 章是 Design Token 的唯一视觉依据；第 13 章给分阶段验收标准。",
               "锚点 #p1–#p15 · 与下方原型锚点在同一文件内", "章节 ch1 ch15 索引 锚点 数据设计 路线 验收", "2026-11-09",
               "15 章方案锚点速查"))
    screens = ["首次启动 S01", "登录解锁 S02", "工作台 S03", "字段配置 S04", "邮件组 S05", "成员弹窗 S06",
               "月度数据 S07", "导入向导 S08", "空模板导出 S09", "模板编辑器 S10", "发送前核对 S11",
               "发送队列 S12", "历史日志 S13", "设置 S14", "全局状态 S15"]
    ssubs = " · ".join(a(design, s, "s" + str(i + 1), "sub") for i, s in enumerate(screens))
    p.append(E("原型速查", "b-other", '<span>15 屏高保真原型速查</span><span class="sub">　' + ssubs + "</span>",
               "S01/S02 是 TC-103 的界面依据，S04 是 TC-200 字段默认集来源，S10 是 TC-202 模板编辑器依据，"
               "S09 对应 TC-301 空模板导出，S12/S13 对应 P4 发送队列与日志。原型里的示例数据不是真实工资口径。",
               "锚点 #s1–#s15 · v1 只交付深色主题，浅色延后 v1.1", "原型 S01 S02 S04 S10 S09 界面 深色 设计", "2026-11-09",
               "15 屏高保真原型速查"))
    cats.append(("02", "设计方案与界面原型", "cat-02", "st-ok", "✅ 已建成", "".join(p)))

    # ── 03 任务规格 Spec（目录扫描） ─────────────────────────────────────
    p = []
    for f in sorted((ROOT / "docs/specs").glob("TC-*.md")):
        cid = f.name[:6]
        rel = "docs/specs/" + f.name
        note = SPEC_NOTES.get(cid, "（待补核心口径：新增卡片后请在 build-master-index.py 的 SPEC_NOTES 里补一句话说明）")
        crit = SPEC_CRITERIA.get(cid)
        st = next((r["status"] for r in TASKS if r["id"] == cid), "pending")
        meta = "状态：" + st + (" · 验收 " + crit if crit else " · 逐条实测见文末评审记录")
        meta += " · " + stamp(rel) + " 最后更新 · 编号一经发出不复用"
        drift = ""
        self_st = SPEC_SELF_STATUS.get(cid)
        if self_st and self_st != st:
            drift += '<b style="color:#e05a4f"> · ⚠ spec 自报 ' + esc(self_st) + "，看板记 " + esc(st) + "（以看板为准）</b>"
        if not next((r["spec"] for r in TASKS if r["id"] == cid), ""):
            drift += '<b style="color:#e05a4f"> · ⚠ 看板 spec 列未回填</b>'
        p.append(E("Spec", "b-plan", a(rel, f.name), esc(note), esc(meta) + drift,
                   "spec 任务卡 验收标准 " + cid + " " + f.stem, stamp(rel)))
    p.append('<div class="callout c-green"><b class="lbl">验收口径</b>每张卡都先跑 <code>bash scripts/gate.sh</code> 全绿，'
             '再逐条对照「验收标准」记下<b>实际观察到的结果</b>（不写「已验证」）。未过项要么当场修，'
             '要么写清「为什么可以带着它结案 + 由哪张卡补」，然后才发下一张卡。'
             '验收中发现 spec 本身写歪了，先改 spec 并在评审记录注明，不要用改代码去迁就错误的标准。</div>')
    cats.append(("03", "任务规格 Spec", "cat-03", "st-ok", "✅ 逐卡验收", "".join(p)))

    # ── 04 架构决策 ADR（目录扫描） ──────────────────────────────────────
    p = []
    for f in sorted((ROOT / "docs/adr").glob("0*.md")):
        num = f.name[:4]
        rel = "docs/adr/" + f.name
        note = ADR_NOTES.get(num, "（待补决策摘要：新增 ADR 后请在 build-master-index.py 的 ADR_NOTES 里补一句话说明）")
        p.append(E("ADR", "b-rpt", a(rel, f.name), esc(note),
                   esc("accepted · " + stamp(rel) + " · 只追加不修改；不同意就写新 ADR 去 supersede"),
                   "ADR 决策 取舍 代价 " + num + " " + f.stem, stamp(rel)))
    p.append('<div class="callout c-amber"><b class="lbl">ADR 只追加不修改</b>决策时的现场信息只有当时的人知道。'
             '后来者若不同意某条 ADR，写一条新的去 supersede 它，不要改旧的。'
             '已结案卡的验收结论写在 spec 的「评审记录」里，不回填进 ADR 正文。</div>')
    cats.append(("04", "架构决策 ADR", "cat-04", "st-ok", "✅ 全部 accepted", "".join(p)))

    # ── 05 看板、门禁与工程脚本 ──────────────────────────────────────────
    p = []
    p.append(E("任务看板", "b-plan", a("docs/TASKBOARD.md", "docs/TASKBOARD.md · 任务看板（状态唯一事实来源）"),
               str(len(TASKS)) + " 张卡的状态、依赖、spec 与相关 ADR；「当前焦点」逐卡记录结案证据与实测数字"
               "（本次新增 TC-301/302/303 与 TC-400/401/402/403 共 7 张结案记录）；另有结案遗留、外部输入阻塞、依赖说明三张表。"
               "本总纲的分期任务表与进度数字每次生成时都从这里读取。",
               "最后更新 " + stamp("docs/TASKBOARD.md") + " · 状态含 accepted / in_progress / pending / blocked / withdrawn"
               + (" · 本次检出 " + str(len(ISSUES)) + " 处与 spec 的漂移，见 07 分类" if ISSUES else ""),
               "看板 状态 进度 依赖 阻塞 遗留 当前焦点 taskboard", stamp("docs/TASKBOARD.md")))
    p.append(E("门禁", "b-red", a("payslip-mailer/scripts/gate.sh", "payslip-mailer/scripts/gate.sh · 统一验收门禁"),
               "所有卡共用同一组检查（当前 9 项），避免每张卡自己发明一套「合格」。其中两条是本项目专属红线："
               "组件层不得硬编码色值（例外须写 no-token 注释）、Token 引用必须能解析到声明；另有敏感数据未进 git 一条。",
               "bash scripts/gate.sh（全量）/ --fast（只跑秒级检查）· CI 也只调这一份定义", "门禁 gate 色值 token clippy 测试 敏感数据 CI", "2026-11-11"))
    p.append(E("发布脚本", "b-other",
               a("payslip-mailer/scripts/release.sh", "release.sh") + '　<span class="sub">'
               + a("payslip-mailer/scripts/sign.sh", "sign.sh", cls="sub") + " · "
               + a("payslip-mailer/scripts/notarize.sh", "notarize.sh", cls="sub") + " · "
               + a("payslip-mailer/scripts/make-dmg.sh", "make-dmg.sh", cls="sub") + " · "
               + a("payslip-mailer/scripts/env.sh", "env.sh", cls="sub") + "</span>",
               "npm run release 走未签名构建 → hdiutil 制 dmg → SHA-256（决策 D8 的默认路径）；"
               "release --signed 保留签名公证链路但当前无证书、不购买。",
               "ADR-0002 / ADR-0008 · P0 已用这条链路产出 薪邮-0.1.0.dmg", "发布 release dmg hdiutil 签名 公证 SHA256", "2026-11-11"))
    p.append(E("开发循环", "b-other",
               a("payslip-mailer/scripts/dev-run.sh", "dev-run.sh") + '　<span class="sub">'
               + a("payslip-mailer/scripts/setup-dev-keychain.sh", "setup-dev-keychain.sh", cls="sub") + " · "
               + a("payslip-mailer/scripts/setup-dev-signing.sh", "setup-dev-signing.sh", cls="sub") + " · "
               + a("payslip-mailer/scripts/sign-dev.sh", "sign-dev.sh", cls="sub") + "</span>",
               "解决「每次启动都要重新授权钥匙串」：开发构建改用专用钥匙串文件，把编译 → 签名 → 起 app → 自动授权串成一条命令。"
               "根因（未签名二进制的 cdhash / 需 Apple 锚定签名）与被排除的三条路记在 ADR-0009 补充章节。",
               "2026-11-11 实测通过 · 发布构建不受影响，仍走登录钥匙串 + 默认 ACL", "钥匙串 开发 授权 弹窗 dev-run ADR-0009", "2026-11-11"))
    p.append(E("CI", "b-other", a(".github/workflows/release.yml", ".github/workflows/release.yml · CI 测试与发布"),
               "push / PR 到 main 触发 macos-14 门禁；workflow_dispatch 带 release 开关才执行签名公证并产 dmg。"
               "门禁只调 gate.sh 一份定义，避免 CI 绿了但两条专属红线没守住。",
               "⚠️ 仓库目前仍是零 commit，CI 从未首跑；首次 push 后需复核 headless 会话里的 SKIP 分支", "CI github actions 门禁 发布 零commit", "2026-11-11"))
    p.append(E("工程说明", "b-doc",
               a("payslip-mailer/README.md", "payslip-mailer/README.md") + '　<span class="sub">'
               + a("payslip-mailer/package.json", "package.json", cls="sub") + " · "
               + a("payslip-mailer/src-tauri/Cargo.toml", "Cargo.toml", cls="sub") + "</span>",
               "技术栈矩阵、目录结构、首次准备、日常命令（dev / build / typecheck / rust:check / lint / test / gate / release）、"
               "未签名分发放行要点、界面规范与红线。Cargo.toml 里能直接核对全部 Rust 依赖与版本对齐结果。",
               "脚手架实际版本比设计文档略新（React 19.1 / TS 6.0 / Vite 8 / edition 2024），属正常演进", "命令 依赖 Cargo package 目录结构 快速开始", "2026-11-11"))
    p.append(E("生成器", "b-other", a("docs/build-master-index.py", "docs/build-master-index.py · 本页生成器"),
               "扫描 docs/specs 与 docs/adr、解析 TASKBOARD 表格、自动编码并校验所有相对链接，重跑即可让总纲跟上源文档；"
               "还会对照 spec 自报状态与看板，把漂移列进 07 分类。新增卡片只需在脚本里补一句核心口径，"
               "未补的会显式标注「待补」而不是静默漏掉。",
               "python3 docs/build-master-index.py · " + BUILT + " 建立 · " + UPDATED + " 增漂移检出", "生成器 维护 自动 索引 脚本 python 漂移", UPDATED))
    cats.append(("05", "看板、门禁与工程脚本", "cat-05", "st-ok", "✅ 可运行", "".join(p)))

    # ── 06 代码与构建产物 ────────────────────────────────────────────────
    p = []
    p.append(E("Rust 后端", "b-other",
               a("payslip-mailer/src-tauri/src/lib.rs", "src-tauri/src/ · Rust 分层") + '　<span class="sub">'
               + a("payslip-mailer/src-tauri/src/commands/mod.rs", "commands/", cls="sub") + " · "
               + a("payslip-mailer/src-tauri/src/domain/mod.rs", "domain/", cls="sub") + " · "
               + a("payslip-mailer/src-tauri/src/infra/mod.rs", "infra/", cls="sub") + " · "
               + a("payslip-mailer/src-tauri/src/error.rs", "error.rs", cls="sub") + "</span>",
               "commands/ 是 IPC 边界（参数校验 + DTO），domain/ 放实体、状态机、公式与校验规则，infra/ 放 SQLCipher、Keychain、"
               "备份、唤醒、销毁等系统能力；error.rs 统一错误码到中文文案。外部副作用一律留在这一层，前端拿不到工资数值与凭据。",
               "Rust edition 2024 / MSRV 1.88 · 对应设计文档 ch.4 分层", "Rust commands domain infra 分层 Tauri 后端", "2026-11-12"))
    acc = "payslip-mailer/src-tauri/src/"
    p.append(E("验收断言", "b-plan",
               a(acc + "acceptance.rs", "acceptance.rs · 可跑断言") + '　<span class="sub">'
               + a(acc + "acceptance_export.rs", "export", cls="sub") + " · "
               + a(acc + "acceptance_import.rs", "import", cls="sub") + " · "
               + a(acc + "acceptance_verify.rs", "verify", cls="sub") + " · "
               + a(acc + "acceptance_sendlog.rs", "sendlog", cls="sub") + "</span>",
               "把验收标准做成可跑断言：TC-103 的 15 条起头，P3 的导出 / 导入、P4 的三重核对与凭证留痕各有独立断言文件；"
               "每个测试的文档注释写明对应 spec 第几条，随 cargo test 进 CI ——「验收标准」从此不只是文档里的一段话。",
               "TC-103 建立，P3 / P4 扩充为 5 个文件 · 另有 tests/wake_delivery.rs 做跨进程唤醒投递（harness=false）",
               "验收 断言 测试 acceptance cargo test", stamp(acc + "acceptance_sendlog.rs")))
    p.append(E("数据迁移", "b-other",
               a("payslip-mailer/src-tauri/migrations/0001_baseline.sql", "migrations/ · 版本化迁移") + '　<span class="sub">'
               + a("payslip-mailer/src-tauri/migrations/0009_identity_verified.sql", "0009", cls="sub") + " · "
               + a("payslip-mailer/src-tauri/migrations/0010_smtp_settings.sql", "0010", cls="sub") + "</span>",
               "0001–0010 共 10 个 SQL，按 TC-100 的 checksum + 事务回滚机制逐版演进；"
               "0009 补 TC-401 漏持久化的 identity_verified，0010 是 TC-500 的 smtp_settings（凭据只存 Keychain 条目名）。",
               "与设计文档 ch.6 表定义对应 · 迁移失败转只读，不静默建空库", "迁移 SQL schema 表结构 checksum 版本 0010", "2026-11-12"))
    p.append(E("SMTP 引擎", "b-other",
               a(acc + "infra/mailer.rs", "infra/mailer.rs") + '　<span class="sub">'
               + a("payslip-mailer/src/screens/SettingsPanel.tsx", "SettingsPanel.tsx", cls="sub") + " · "
               + a(acc + "infra/batch.rs", "infra/batch.rs", cls="sub") + "</span>",
               "TC-500 的接线点：lettre + rustls 的真 Sender、EmailMessage 构建（multipart/alternative）、自检信测试连接、"
               "S14「邮件设置」段；infra/batch.rs 保留注入 Sender 的编排循环，自动化验收走 StubTransport 而绝不真发"
               "（设计文档 ch.3 / ADR-0027）。",
               "2026-11-12 随 TC-500 发出 · 真凭据真机验证归 TC-501", "SMTP lettre rustls StubTransport 邮件设置 S14 mailer", "2026-11-12"))
    p.append(E("唤醒集成测试", "b-other", a("payslip-mailer/src-tauri/tests/wake_delivery.rs", "tests/wake_delivery.rs"),
               "验证「另一个进程发 → 收到 → 连接关掉」整条投递链。libtest 把测试体放在工作线程上，主线程 runloop 泵不到，"
               "所以这条用例必须自带 main()；环境不满足时打印 SKIP 退出 0，不挂住也不假绿。",
               "PAYSLIP_WAKE_TEST_SKIP=1 可当场复现 SKIP 分支 · 首次 push 后看 CI 首跑", "唤醒 集成测试 runloop SKIP headless", "2026-11-11"))
    p.append(E("前端", "b-other",
               a("payslip-mailer/src/App.tsx", "src/ · React 前端") + '　<span class="sub">'
               + a("payslip-mailer/src/styles/tokens.css", "tokens.css", cls="sub") + " · "
               + a("payslip-mailer/src/components/ui/index.ts", "components/ui/", cls="sub") + " · "
               + a("payslip-mailer/src/lib/api.ts", "lib/api.ts", cls="sub") + "</span>",
               "screens/ 是 15 屏界面（SetupWizard / LockScreen / FieldConfig / MemberConfig / TemplateEditor / PeriodPanel / BackupPanel 等）；"
               "P3 新增 ExportPanel / ImportWizard / RecordsTable，P4 新增 VerifyPanel，S14 SettingsPanel 接 TC-500 的邮件设置段。"
               "components/ 是设计与布局基元，tokens.css 是全部色值的唯一来源，api.ts 是前端到 Rust Command 的唯一通道。",
               "React 19.1 + TypeScript 6 + Vite 8 + CSS Modules · v1 深色唯一", "React 前端 screens 组件 token css modules", "2026-11-12"))
    p.append(E("配置", "b-other",
               a("payslip-mailer/src-tauri/tauri.conf.json", "tauri.conf.json") + '　<span class="sub">'
               + a("payslip-mailer/src-tauri/capabilities/default.json", "capabilities/", cls="sub") + " · "
               + a("payslip-mailer/.cargo/config.toml", ".cargo/config.toml", cls="sub") + "</span>",
               "窗口尺寸（默认 1200×800 / 最小 1024×680）、bundle id、权限白名单都在 tauri.conf.json 与 capabilities 里；"
               ".cargo/config.toml 把 SDKROOT 钉回 CLT SDK（ADR-0003），日后装了完整 Xcode 需删掉。",
               "bundle id cn.payslipmailer.desktop → 数据目录跟 app_data_dir() 走（ADR-0012 §1）", "配置 tauri 权限 capabilities SDKROOT bundle id", "2026-11-11"))
    p.append(E("构建产物", "b-data",
               a("payslip-mailer/artifacts/薪邮-0.1.0.dmg", "artifacts/薪邮-0.1.0.dmg") + '　<span class="sub">'
               + a("payslip-mailer/artifacts/薪邮-0.1.0.dmg.sha256", "SHA-256", cls="sub") + " · "
               + a("payslip-mailer/dist/index.html", "dist/", cls="sub") + "</span>",
               "P0 已产出未签名 dmg 与校验和；dist/ 是前端构建产物（git 忽略）。spctl -a -vv 预期失败 —— 未签名符合 D8，"
               "不是构建坏了。真正的产品交付仍要等 P6 的安装说明与升级演练。",
               "0.1.0 · 2026-11-11 产出 · 内部分发以 U 盘 / 局域网共享为推荐路径", "dmg 产物 sha256 未签名 分发 artifacts dist", "2026-11-11"))
    cats.append(("06", "代码与构建产物", "cat-06", "st-ok", "✅ 持续生长", "".join(p)))

    # ── 07 红线、阻塞与遗留 ──────────────────────────────────────────────
    p = []
    p.append(E("敏感数据", "b-red", a(".gitignore", ".gitignore · 敏感数据红线"),
               "成员名单（members/roster/payroll 系列）、真实工资数据、密钥与凭据、日志一律不入库；"
               ".workbuddy-ai/ 与构建产物同样排除。门禁里另有「敏感数据未进 git」一条做兜底。",
               "2026-11-11 建立 · 名单与真实工资表只放本机，永不进版本库", "gitignore 敏感 名单 工资 凭据 红线", "2026-11-11"))
    p.append('<div class="callout c-blue"><b class="lbl">八条开发红线（每张卡的范围里默认包含，不重复写）</b>'
             '<ol style="margin-bottom:0">'
             '<li>绝不把多个收件人放进同一封邮件；禁止 CC / BCC。</li>'
             '<li>绝不把工资数值写进日志、错误上报、备份文件名。</li>'
             '<li>绝不提供「一键群发全部组」而不经过二次确认的入口。</li>'
             '<li>失败重试只允许针对<b>未成功的 job</b>，不提供整批重发。</li>'
             '<li>前端拿不到工资数值与凭据的长期引用；所有外部副作用走 Rust Command 层。</li>'
             '<li>模板引擎沙箱化，禁止文件读取 / 系统调用 / 访问环境变量。</li>'
             '<li>已发送账套只读；修改必须「解锁 + 生成修订记录 + 仅对未发送者补发」。</li>'
             '<li>字段一旦被历史数据引用只能停用不能删除；月份账套冻结 field_snapshot。</li>'
             '</ol></div>')
    p.append('<table><tr><th>已定决策</th><th>内容</th><th>对开发的约束 / 代价</th></tr>'
             '<tr><td>D1</td><td>Tauri 2 + Rust + React/TS + Vite</td><td>已锁定，不可再评估其他方案</td></tr>'
             '<tr><td>D2</td><td>飞书邮箱 smtp.feishu.cn:465 + 专用密码</td><td>OAuth2 仅作扩展；设置页要有专用密码失效引导</td></tr>'
             '<tr><td>D3</td><td>v1 只做深色主题</td><td>全部颜色走 CSS 变量，浅色延后 v1.1</td></tr>'
             '<tr><td>D4</td><td>v1 不做 PDF 附件</td><td>正文即工资条；省掉渲染 / 口令分发 / 临时明文清理三条链路</td></tr>'
             '<tr><td>D5</td><td>团队模式 A：一台管理机发全公司</td><td>单管理员 + 单机加密库；不引入共享存储 / RBAC / 并发锁</td></tr>'
             '<tr><td>D6</td><td>真实发件账号后置，P0–P4 全走 mock SMTP</td><td>测试环境绝不真发；P5 再做真实联调</td></tr>'
             '<tr><td>D7</td><td>不上 Mac App Store，内部分发 dmg</td><td>不需要 App Sandbox</td></tr>'
             '<tr><td>D8</td><td>不购买 Apple Developer Program</td><td>未签名 + 本地构建/U 盘分发；代价是手动放行与升级后 Keychain 授权重置</td></tr>'
             '</table>')
    p.append('<div class="callout c-amber"><b class="lbl">外部输入阻塞（不是工程问题，催业务方）</b>'
             '<table style="margin-bottom:0"><tr><th>缺什么</th><th>挡住</th><th>说明</th></tr>'
             '<tr><td>飞书发件账号 + 专用密码</td><td>真机验证（看板记「TC-500 及之后全部」）</td><td>必须是真实分配了企业邮箱席位的成员账号，不能用邮件组；管理员需开启「第三方邮箱客户端登录」。<b>TC-500 的新 spec 已把真凭据验证划给 TC-501，本卡改跑 StubTransport 验收</b>（ADR-0027）——看板此行尚未同步</td></tr>'
             '<tr><td>成员名单（姓名/邮箱/工号/组）</td><td>TC-201 的真实数据验证</td><td>机制已验、名单不是代码问题；只放本机，绝不进 git</td></tr>'
             '<tr><td>公司名 + 回信邮箱 + 邮件文案</td><td>TC-202 的默认内容</td><td>设计文档第 8 章有示例稿可作 v1 初版</td></tr>'
             '<tr><td>真实工资条字段口径</td><td>已澄清，不再阻塞</td><td>字段是<b>用户数据</b>（S04 让 HR 自己配），TC-200 已按原型示例集落地，用户可随时增删停用</td></tr>'
             '</table></div>')
    if ISSUES:
        p.append('<div class="callout c-amber"><b class="lbl">源文档一致性（本次生成自动检出 '
                 + str(len(ISSUES)) + ' 处）</b>'
                 '生成器会对照每份 spec 自报的状态与看板表格，只报告、不擅自改任何一份事实来源：'
                 '<ul style="margin-bottom:0">' + "".join("<li>" + x + "</li>" for x in ISSUES) + '</ul>'
                 '本页的分期任务表与进度数字<b>一律按 <code>docs/TASKBOARD.md</code> 显示</b>；'
                 '确认哪边是对的之后，改看板（或改 spec 的状态行）再重跑生成器即可。'
                 '这类漂移不消除，规格覆盖率与卡片状态就会各说各话。</div>')
    p.append('<div class="callout c-green"><b class="lbl">结案遗留（摘选，完整表见 ' + tb_link("docs/TASKBOARD.md") + '）</b>'
             '<table style="margin-bottom:0"><tr><th>来源</th><th>遗留</th><th>落到哪张卡</th></tr>'
             '<tr><td>TC-103 · 标准 11/12/19</td><td>合盖唤醒、空闲自动锁屏、真人确认红绿灯与窗口标题，三条必须使用者本人在真机做</td><td>TC-103 收口（in_progress）</td></tr>'
             '<tr><td>TC-102 集成测试</td><td>wake_delivery 的投递断言只在有窗口会话的开发机真的执行过；CI 首跑期望 SKIP 且退出 0</td><td>首次 commit 后 CI 首跑；若 FAIL 回 ADR-0011 §10 重跑四组对照，别先改测试</td></tr>'
             '<tr><td>TC-101</td><td>真实 ACL 重置（升级后签名变了）在临时钥匙串里测不到</td><td>TC-601 真机演练</td></tr>'
             '<tr><td>TC-000 标准 10</td><td>原生窗口像素截图未取到，后台 shell 起 app 时 System Events 看不到窗口</td><td>已转成 TC-103 标准 19</td></tr>'
             '<tr><td>门禁</td><td>门禁项编号会随加项漂移，spec 里旧的「第 5 项 / 第 9 项」已不准</td><td>以后一律按<b>名字</b>引用（如「Rust 静态检查」「加密栈链接」）</td></tr>'
             '</table></div>')
    p.append('<div class="callout c-blue"><b class="lbl">尚未解决 / 持续跟踪</b>浅色主题（v1.1）、'
             'Gmail/M365 OAuth2（可先只做授权码）、多机备份交接流程与保管责任（模式 A 下优先级低）、'
             'tauri-plugin-updater 自动更新（v2，需签名更新清单 + HTTPS 源）、'
             '仓库零 commit 待使用者点头后首提交（CI 首跑随之补上）。详见 '
             + tb_link("PREP-CHECKLIST.md") + " §6。</div>")
    cats.append(("07", "红线、阻塞与遗留", "cat-07", "st-red", "🔴 有硬阻塞", "".join(p)))

    # ── ⚙ 维护规范与履历 ────────────────────────────────────────────────
    p = []
    p.append('<div class="callout c-blue"><b class="lbl">登记规则（本项目所有会话共同遵守）</b>'
             '① 新产出的任何交付文件，落盘后登记到对应分类，写清「分类徽章 + 文件链接 + 一句话说明 + 版本与日期」；<br>'
             '② 任务卡状态只改 <code>docs/TASKBOARD.md</code>，然后重跑生成器 —— 不要手改本页表格；<br>'
             '③ 新增 spec / ADR 由目录扫描自动登记，须在生成器的 NOTES 里补一句核心口径，未补会标「待补」；<br>'
             '④ ADR 只追加不修改，不用改旧决策去迁就新实现；作废的卡标 withdrawn 留在看板上，编号不复用不重排；<br>'
             '⑤ 成员名单、真实工资表、凭据、日志<b>永不登记</b>（也不入库），见 .gitignore；<br>'
             '⑥ 本页不产生新决策，不改写验收标准；与源文档冲突时以源文档为准；<br>'
             '⑦ 文件命名沿用既有约定：<code>TC-xxx-标题.md</code> / <code>0xxx-决策.md</code>，日期写在文档内而不是文件名里。</div>')
    p.append('<div class="callout c-green"><b class="lbl">分类定义</b>'
             '<b style="display:inline">01 宪法与入口</b>＝项目是什么、怎么开始；'
             '<b style="display:inline">02 设计与原型</b>＝做成什么样（15 章 + S01–S15）；'
             '<b style="display:inline">03 Spec</b>＝每张卡怎么算过；'
             '<b style="display:inline">04 ADR</b>＝为什么这么选、放弃了什么；'
             '<b style="display:inline">05 看板与工程脚本</b>＝现在在做什么、怎么判合格；'
             '<b style="display:inline">06 代码与产物</b>＝落地的东西在哪；'
             '<b style="display:inline">07 红线 / 阻塞 / 遗留</b>＝不能碰什么、还差什么。</div>')
    p.append('<table><tr><th>维护履历</th><th>日期</th><th>内容</th></tr>'
             '<tr><td>V1.0</td><td>' + BUILT + '</td><td>参照 AgentTeam 的总纲形态建立本页：进度看板 + P0–P6 分期任务表（状态取自 TASKBOARD）'
             '+ 7 个文件分类 + 双层检索 + 最新更新抽屉 + 登记规范；'
             '把散在 README / PREP-CHECKLIST / TASKBOARD / 19 条 ADR 里的口径收拢成一份可读索引。'
             '同时落 <code>docs/build-master-index.py</code> 使状态与文件清单可重复生成。'
             '附带整理：README 进度表同步为看板实际状态并接入总纲入口，docs/README 新增 §0 说明边界。'
             '本版<b>不改动任何决策与验收标准</b>。</td></tr>'
             '<tr><td>V1.1</td><td>' + UPDATED + '</td><td>按 TASKBOARD 的新基线同步：'
             'P3（TC-301~303）与 P4（TC-400~403）共 7 张卡结案，accepted 由 11 增至 18；'
             '补登记 8 份新 spec 与 8 条新 ADR（0020–0027），刷新里程碑轨道、chipline、统计数字与 06 分类的代码登记；'
             '新增「源文档一致性」自动检出（spec 自报状态 / 看板 spec 列回填）；README 进度表同步到最新。'
             '本版<b>仍不改动任何决策与验收标准</b>。</td></tr></table>')
    p.append('<div class="callout c-amber"><b class="lbl">一个刻意的差异</b>参考项目的总纲是「唯一事实来源」，'
             '本项目的 <code>docs/</code> 才是唯一事实来源，总纲只是它的投影。原因很简单：这里每张卡都要逐条按 spec 验收，'
             '一旦总纲里出现第二份可编辑的状态副本，早晚会有两处不一致。所以本页的任务表、进度数字、'
             'spec/ADR 清单全部由生成器读源文档得出 —— 想改状态，请去改 TASKBOARD。</div>')
    cats.append(("⚙", "总纲维护规范与履历", "cat-meta", "st-meta", "📋 治理", "".join(p)))
    return cats


# ─────────────────────────────────────────────────────────────────────────────
# 页面组装
# ─────────────────────────────────────────────────────────────────────────────
def hero() -> str:
    out = ['<header class="hero"><h1>薪邮 Payslip Mailer · 项目总纲</h1>']
    out.append('<div class="meta"><span>版本：' + VERSION + '</span><span>建立：' + BUILT + '</span>'
               '<span>状态基线：' + UPDATED + ' 的 docs/TASKBOARD.md</span>'
               '<span>性质：本项目唯一入口，全部产出统一登记于此</span></div>')
    out.append('<div class="tools">'
               '<button id="expand-all" type="button">展开全部</button>'
               '<button id="collapse-all" type="button">折叠全部</button>'
               '<button id="theme-btn" type="button">☀ 浅色</button></div>')
    out.append('<div class="oneliner" style="border-color:var(--accent-line)"><b>外部文档声明</b>：'
               '本文件独立于应用本身 —— 不进 App、不由后端提供、不参与构建、不打进 dmg；'
               '直接以浏览器打开本文件查阅（<code>file://</code> 即可，内部链接均为相对路径，无任何外链与 CDN）。</div>')
    out.append('<div class="oneliner"><b>一句话目标</b>：给中小团队 HR/财务做一台<b>本机</b>的发薪最后一公里工具 —— '
               '配置一次 → 每月导出空 Excel 模板 → 财务填好导入 → 逐人独立成信发工资条 → 全程留痕可举证。'
               '数据不出本机（SQLCipher + macOS Keychain），零抄送零密送，v1 只做深色，不上 App Store。</div>')
    out.append('<div class="oneliner">本页上半部分是<b>进度与 P0–P6 分期任务看板</b>，下半部分按分类登记全部产出；'
               '支持<b>全文检索</b>（跨分类）与<b>分类内检索</b>，各分类可折叠，右下角「最新更新」按条目日期自动汇总。'
               '状态与文件清单由 <code>docs/build-master-index.py</code> 从源文档生成，<b>不手抄第二份状态</b>。</div>')
    out.append("</header>")
    return "".join(out)


def rail() -> str:
    out = ['<div class="rail" id="rail">']
    for code, name, cap in PHASES:
        rows = [r for r in TASKS if phase_of(r["id"]) == code]
        sts = [r["status"] for r in rows]
        if sts and all(s == "accepted" for s in sts):
            cls, nd = "done", "✅ 全部结案"
        elif sts and all(s == "blocked" for s in sts):
            cls, nd = "blocked", "🔴 外部阻塞"
        elif any(s in ("accepted", "in_progress", "review") for s in sts):
            cls, nd = "doing", "🔄 推进中"
        else:
            cls, nd = "", "⬜ 未开始"
        out.append('<div class="node ' + cls + '"><div class="b"></div>'
                   '<div class="nl">' + esc(code) + "<br>" + esc(name) + "</div>"
                   '<div class="nd">' + esc(nd) + "</div></div>")
    out.append("</div>")
    out.append('<div class="rail-cap"><b>P0 / P2 / P3 / P4 已全部结案</b> —— 月度数据闭环与 Rust 侧发送链路均已落地；'
               'P1 只剩 TC-103 的 3 条真人真机验收；P5 的 TC-500 已发出 spec 与 ADR-0027，'
               '真机联调仍等飞书专用密码；P6 未开始。阶段顺序依据 '
               + a("PREP-CHECKLIST.md", "PREP-CHECKLIST §4") + "。</div>")
    return "".join(out)


def devboard() -> str:
    out = ['<div class="devboard">',
           '<div class="dbt">开发进度 · P0–P6 任务清单 <small>（点页签切换；'
           '状态每次生成时从 docs/TASKBOARD.md 读取，进度由本页表格实时计算）</small></div>',
           '<div class="tabs" role="tablist">']
    for i, (code, name, _) in enumerate(PHASES):
        out.append('<button class="tab' + (' active' if i == 0 else '') + '" type="button" data-p="' + code.lower() + '">'
                   + esc(code + " " + name) + "</button>")
    out.append("</div>")
    for i, (code, name, cap) in enumerate(PHASES):
        out.append('<div class="panel" id="' + code.lower() + '"' + (" hidden" if i else "") + ">")
        out.append('<div class="sum">计算中…</div><div class="pbar"><i style="width:0%"></i></div>')
        out.append('<table><tr><th>卡号</th><th>任务</th><th>目标 / 验收要点</th><th>依赖</th><th>状态</th></tr>')
        rows = [r for r in TASKS if phase_of(r["id"]) == code]
        for r in rows:
            meta = META.get(r["id"], ("（目标待补：请在生成器的 META 里补一句）", 1))
            label, cls = STATUS.get(r["status"], (r["status"], "bno"))
            if r["spec"]:
                cell = a(r["spec"], r["id"])
                hasspec = "1"
            else:
                cell = a("docs/TASKBOARD.md", r["id"])
                hasspec = "0"
            out.append('<tr data-id="' + r["id"] + '" data-complexity="' + str(meta[1]) + '" data-hasspec="' + hasspec + '">'
                       '<td class="id">' + cell + "</td>"
                       '<td class="g">' + esc(r["title"]) + "</td>"
                       "<td>" + esc(meta[0]) + "</td>"
                       "<td>" + esc(r["deps"]) + "</td>"
                       '<td><span class="' + cls + '">' + esc(label) + "</span></td></tr>")
        if not rows:
            out.append('<tr><td colspan="5" style="color:var(--ink-3)">本阶段暂无任务卡</td></tr>')
        out.append("</table>")
        out.append('<div class="dbnote">' + esc(cap) + " · 逐卡验收证据见 03 分类对应 spec 的「评审记录」；"
                   "完整依赖说明见 " + a("docs/TASKBOARD.md", "TASKBOARD") + "。</div></div>")
    out.append("</div>")
    return "".join(out)


def board() -> str:
    out = ['<div class="board"><div class="board-title"><span class="dot"></span>项目建设进度看板</div>']
    out.append('<section class="overall-progress" id="overall-progress"><h2>项目整体开发进度</h2><div class="progress-grid">')
    for mid, label, hint in [
        ("task-progress", "任务卡完成率", "按整卡 accepted 计"),
        ("spec-progress", "规格覆盖率", "已发出 spec 的卡"),
        ("weighted-progress", "复杂度加权进度", "按任务范围估点数"),
    ]:
        out.append('<div class="progress-metric" id="' + mid + '"><span>' + label + "</span>"
                   "<strong>—</strong><progress max=\"100\" value=\"0\"></progress><small>" + hint + "</small></div>")
    out.append("</div><details><summary>查看计分口径与主要剩余项</summary>"
               "<p>三项指标都由下方 P0–P6 任务表实时算出，不在本页另存一份数字。分子只计 <code>accepted</code> 的整卡；"
               "<code>in_progress</code> / <code>pending</code> / <code>blocked</code> 全部留在分母，所以「阻塞」会如实压低完成率。</p>"
               "<p>加权点数按任务范围估算（1 点局部文案或配置、2 点单模块、3 点跨模块或持久化联调、5 点跨系统或关键流程），"
               "<b>用于看板比重，不是工期承诺</b>。规格覆盖率 = 已发出 spec 的卡 / 全部卡，未发卡的卡号链接回 TASKBOARD。</p>"
               "<p id=\"remaining-heavy\"></p>"
               "<p>状态基线：" + UPDATED + " 的 <code>docs/TASKBOARD.md</code>。TC-103 虽已跑完 18/21 条标准，"
               "剩 3 条必须使用者本人在真机做，因此仍记 <code>in_progress</code>，不提前涂绿。"
               "本次 P3 的 TC-301~303 与 P4 的 TC-400~403 共 7 张卡转为 <code>accepted</code>；规格覆盖率按看板的 spec 列计，"
               "目录里已落盘但看板未回填的 spec 不算进分子"
               + ("（另有 " + str(len(ISSUES)) + " 处源文档漂移，见 07 分类）" if ISSUES else "")
               + "。</p></details></section>")
    out.append(rail())
    out.append(devboard())
    n_adr = len(list((ROOT / "docs/adr").glob("0*.md")))
    n_spec = len(list((ROOT / "docs/specs").glob("TC-*.md")))
    out.append('<div class="stats" id="stats">'
               '<div class="stat"><b id="stFiles">–</b><span>登记文件</span></div>'
               '<div class="stat"><b id="stEntries">–</b><span>登记条目</span></div>'
               '<div class="stat"><b id="stCats">–</b><span>已启用分类</span></div>'
               '<div class="stat"><b>' + str(n_adr) + '</b><span>已定 ADR</span></div>'
               '<div class="stat"><b>' + str(n_spec) + '</b><span>任务规格</span></div></div>')
    out.append('<div class="fill" id="fill"></div>')
    out.append('<div class="chipline">'
               '<span class="kchip g">✅ P0 / P2 / P3 / P4 全部结案：月度数据闭环 + Rust 侧发送链路完成，accepted 11→18</span>'
               '<span class="kchip">🔄 P1 仅剩 TC-103 的 3 条真人真机验收；TC-500 已发出 spec 与 ADR-0027</span>'
               '<span class="kchip r">🔴 P5 真机联调仍等飞书专用密码（TC-501 / 502 未开始）</span>'
               '<span class="kchip">⚠️ 仓库仍是零 commit，CI 首跑待首次 push</span></div>')
    out.append("</div>")
    return "".join(out)


def gsearch() -> str:
    return ('<div class="gsearch"><div class="row">'
            '<input id="gq" type="text" placeholder="🔍 全文检索：跨全部分类搜文件名、说明、关键词（如：Keychain / 快照 / 飞书 / 停用 / TC-300…）">'
            '<button id="gclear" type="button">清除</button></div>'
            '<div id="ghint">输入即跨分类过滤并自动展开命中分类；清空恢复默认视图（全部分类折叠）。</div></div>')


def categories_html() -> str:
    out = []
    for no, name, cid, chipcls, chiptext, body in build_categories():
        out.append("<!-- ===== " + no + " " + name + " ===== -->")
        out.append('<section class="cat collapsed" id="' + cid + '" data-name="' + esc(name) + '">')
        out.append('<div class="sec-head"><button class="tgl" type="button">▸</button>'
                   '<span class="no">' + esc(no) + "</span><h2>" + esc(name) + "</h2>"
                   '<span class="cnt"></span><span class="stchip ' + chipcls + '">' + esc(chiptext) + "</span>")
        if cid != "cat-meta":
            out.append('<input class="ssearch" type="text" placeholder="本分类内检索…">')
        out.append("</div><div class=\"sec-body\">" + body
                   + '<div class="nomatch">本分类内无匹配条目</div></div></section>')
    return "\n".join(out)


def build() -> str:
    out = ['<!DOCTYPE html>', '<html lang="zh-CN">', '<head>', '<meta charset="UTF-8">',
           '<meta name="viewport" content="width=device-width, initial-scale=1.0">',
           '<title>薪邮 Payslip Mailer · 项目总纲</title>', "<style>", CSS, "</style>", "</head>", "<body>",
           '<div class="wrap">', hero(), board(), gsearch(), categories_html()]
    out.append('<footer>薪邮 Payslip Mailer · 项目总纲 ' + VERSION + " · " + BUILT
               + " · 单文件自包含，无外链 / 无 CDN / 无遥测 · 由 docs/build-master-index.py 生成</footer>")
    out.append("</div>")
    out.append('<button id="dw-fab" type="button">🕘 最新更新</button><div id="dw-mask"></div>'
               '<div id="upd-drawer"><div class="dw-head"><b>最近登记的产出</b>'
               '<button id="dw-close" type="button">✕</button></div>'
               '<div class="dw-tip">按条目日期自动排序，登记即纳入，无需手工维护。</div>'
               '<div id="drawer-list"></div></div>')
    out.append(JS)
    out.append("</body></html>")
    return "\n".join(out)


def main() -> None:
    if not TASKS:
        raise SystemExit("未能从 docs/TASKBOARD.md 解析出任务卡，停止生成以免产出空看板。")
    html = build()
    OUT.write_text(html, encoding="utf-8")
    uniq = sorted(set(WARN))
    print("已生成：" + str(OUT))
    print("任务卡 " + str(len(TASKS)) + " 张 · 阶段 " + str(len(PHASES)) + " 个 · "
          + "HTML " + str(len(html)) + " 字符")
    if uniq:
        print("⚠️ 以下链接指向不存在的文件，请核对：")
        for w in uniq:
            print("   - " + w)
    else:
        print("✅ 全部相对链接校验通过，零断链")
    if ISSUES:
        print("⚠️ 源文档一致性 " + str(len(ISSUES)) + " 处（已写入总纲 07 分类，未改动任何源文档）：")
        for x in ISSUES:
            print("   - " + re.sub(r"<[^>]+>", "", x))


if __name__ == "__main__":
    main()
