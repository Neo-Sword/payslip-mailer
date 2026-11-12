import { useCallback, useEffect, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { AppShell, PaneHead } from "./components/layout/AppShell";
import { Stage } from "./components/layout/Stage";
import {
  Button,
  Card,
  Chip,
  Column,
  ConfirmDialog,
  Empty,
  Progress,
  Stat,
  StatGrid,
  Steps,
  Table,
  ToastProvider,
  useToast,
} from "./components/ui";
import {
  adminStatusForDisplay,
  api,
  BackendError,
  isDesktop,
  type AdminState,
  type AppInfo,
} from "./lib/api";
import { KeychainGuide } from "./screens/KeychainGuide";
import { LockScreen } from "./screens/LockScreen";
import { ReadOnlyBanner } from "./screens/ReadOnlyBanner";
import { SettingsPanel } from "./screens/SettingsPanel";
import { RestoreDialog } from "./screens/RestoreDialog";
import { FieldConfig } from "./screens/FieldConfig";
import { MemberConfig } from "./screens/MemberConfig";
import { TemplateEditor } from "./screens/TemplateEditor";
import { PeriodPanel } from "./screens/PeriodPanel";
import { ExportScreen } from "./screens/ExportScreen";
import { ImportWizard } from "./screens/ImportWizard";
import { VerifyPanel } from "./screens/VerifyPanel";
import { QueuePanel } from "./screens/QueuePanel";
import { HistoryPanel } from "./screens/HistoryPanel";
import { SetupWizard } from "./screens/SetupWizard";
import "./styles/tokens.css";
import "./styles/global.css";
import s from "./App.module.css";

/* ============================================================
   三态路由（spec 范围 6 · ADR-0012 §7）
     未初始化        → S01 初始化向导
     已初始化未解锁  → S02 锁屏
     已解锁          → 工作台（S03，数据仍是 mock）
   外加两种「出不来」的状态：
     钥匙串条目丢失  → 专属引导（两条分支都能点）
     其他后端错误    → 说清发生了什么 + 下一步

   锁定的进界面靠**事件**，不靠轮询（ADR-0012 §6）：
   Rust 侧锁定回调 `emit("app-locked")`，前端收到就切锁屏。
   轮询的两个问题：锁定态下库是关的，读不到东西；
   1 秒一次的 IPC 也换不来「立刻」。
   ============================================================ */

type View = "loading" | "setup" | "locked" | "unlocked" | "keychain" | "error";

interface AppInfoState {
  version: string;
  privacyNote: string;
  dataDir: string | null;
}

export default function App() {
  const [view, setView] = useState<View>("loading");
  const [status, setStatus] = useState<AdminState | null>(null);
  const [info, setInfo] = useState<AppInfoState>({
    version: "1.0.0",
    privacyNote: "数据仅保存在本机 · 无遥测 · 无崩溃上报 · 无自动更新检查",
    dataDir: null,
  });
  const [fatal, setFatal] = useState<string | null>(null);
  const [reinit, setReinit] = useState(false);
  /** 「重新初始化」的确认框与执行状态 */
  const [reinitOpen, setReinitOpen] = useState(false);
  const [reinitBusy, setReinitBusy] = useState(false);
  const [reinitError, setReinitError] = useState<string | null>(null);
  /** 恢复对话框由 App 持有：锁屏（忘了口令）与工作台（设置页）两处都要用它 */
  const [restoreOpen, setRestoreOpen] = useState(false);

  /** 读一次状态，按结果决定渲染哪一屏 */
  const refresh = useCallback(async () => {
    try {
      const st = await adminStatusForDisplay();
      setStatus(st);
      setFatal(null);
      if (!st.initialized) {
        setView("setup");
      } else if (!st.unlocked) {
        setView("locked");
      } else {
        setView("unlocked");
      }
    } catch (raw) {
      const err = raw as BackendError;
      if (err.code === "KEYCHAIN_MISSING") {
        setView("keychain");
      } else {
        setFatal(err.message || "读取本机状态失败。");
        setView("error");
      }
    }
  }, []);

  useEffect(() => {
    void refresh();
    api
      .appInfo()
      .then((i: AppInfo) =>
        setInfo({
          version: i.version,
          privacyNote: i.privacy_note,
          dataDir: i.data_dir,
        }),
      )
      .catch(() => {
        /* 取不到就先用默认隐私承诺，不打断启动 */
      });
  }, [refresh]);

  /**
   * 重新初始化：把**读不出来**的旧库移到一边（不删），然后回首启向导。
   *
   * 为什么要有这一步：错误文案写着「请先从备份恢复或重新完成初始化」，
   * 而这两条路原本都没有按钮 —— 使用者被卡在「没能读到本机状态」那一屏，
   * 唯一能按的「重试」永远不会成功（TC-600 真机验收撞到）。
   *
   * 旧库是**改名**移走的（`data.db.bak-<时间戳>`），一个字节都没丢；
   * 钥匙串条目不动（下一次建档会覆盖写它）。
   */
  const startReinitialize = useCallback(async () => {
    setReinitBusy(true);
    setReinitError(null);
    try {
      await api.reinitializeLocalState();
      setReinitOpen(false);
      setReinit(true);
      setView("setup");
    } catch (raw) {
      setReinitError((raw as BackendError).message || "移动旧数据失败，请重试。");
    } finally {
      setReinitBusy(false);
    }
  }, []);

  /* 锁定事件：Rust 侧空闲锁定 / 唤醒锁定 / ⌘L 都会发这一条 */  useEffect(() => {
    if (!isDesktop) return;
    let unlisten: (() => void) | undefined;
    let cancelled = false;
    void listen("app-locked", () => {
      void refresh();
    }).then((fn) => {
      if (cancelled) fn();
      else unlisten = fn;
    });
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, [refresh]);

  /* ⌘L 立即锁定（设计文档 ch.11 快捷键） */
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === "l") {
        e.preventDefault();
        void api
          .lockNow()
          .then(() => refresh())
          .catch(() => {
            /* 锁定失败就没有锁屏可切 */
          });
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [refresh]);

  // 恢复对话框挂在最外层：它跨三态（首启 / 锁屏 / 工作台）都要能用 ——
  // 忘了口令的人停在锁屏上，而那正是唯一需要恢复的时刻。
  const restoreDialog = (
    <RestoreDialog
      open={restoreOpen}
      onClose={() => setRestoreOpen(false)}
      onDone={(outcome) => {
        setRestoreOpen(false);
        void refresh();
        if (outcome.adminEmail) {
          setFatal(null);
        }
      }}
    />
  );

  if (view === "loading") {
    return (
      <div className={s.boot}>
        <span>正在读取本机状态…</span>
        {restoreDialog}
      </div>
    );
  }

  /* 重新初始化的确认框：危险动作要说清「发生了什么 / 影响范围 / 下一步」 */
  const reinitDialog = (
    <ConfirmDialog
      open={reinitOpen}
      title="重新初始化本机数据？"
      danger
      confirmText={reinitBusy ? "正在处理…" : "移到一边并重新建档"}
      description={
        <>
          <b>会发生什么：</b>当前这份打不开的加密库（{info.dataDir ?? "本机数据目录"}/data.db）
          会被<b>改名</b>成 <code>data.db.bak-&lt;时间戳&gt;</code> 留在原目录，
          然后应用回到首启向导，建一套新的库。
          <br />
          <b>影响范围：</b>旧库<b>不会被删除</b>（仍是加密的，改回文件名就能找回）；
          导出的加密备份不受影响；钥匙串条目也不动。
          <br />
          <b>下一步：</b>走完首启向导即可。若你只是想救回旧数据，先用「从备份恢复」。
          {reinitError && (
            <>
              <br />
              <span style={{ color: "var(--line-danger)" }}>{reinitError}</span>
            </>
          )}
        </>
      }
      onConfirm={() => void startReinitialize()}
      onCancel={() => setReinitOpen(false)}
    />
  );

  if (view === "error") {
    return (
      <Stage title="薪邮" titleRight="出了点问题">
        <div className={s.fatal}>
          <h1 className={s.fatalTitle}>没能读到本机状态</h1>
          <p className={s.fatalText}>{fatal}</p>
          <div className={s.fatalActions}>
            <Button variant="primary" onClick={() => void refresh()}>
              重试
            </Button>
            <Button onClick={() => setRestoreOpen(true)}>从备份恢复</Button>
            <Button
              onClick={() => {
                setReinitError(null);
                setReinitOpen(true);
              }}
            >
              重新初始化
            </Button>
          </div>
          <p className={s.fatalFoot}>
            「重新初始化」会把这份打不开的旧库改名移到一边（<b>不删除</b>），然后重新建档。
            旧数据还在，只是当前打不开。
          </p>
        </div>
        {reinitDialog}
        {restoreDialog}
      </Stage>
    );
  }

  if (view === "keychain") {
    return (
      <Stage title="薪邮" titleRight="钥匙串缺少授权">
        <KeychainGuide
          dataDir={info.dataDir}
          onRetrySucceeded={() => void refresh()}
          onReinitialize={() => {
            setReinitError(null);
            setReinitOpen(true);
          }}
        />
        {reinitDialog}
        {restoreDialog}
      </Stage>
    );
  }

  if (view === "setup") {
    return (
      <Stage title="薪邮 · 初始化" titleRight={`v${info.version}`} width={520}>
        <SetupWizard
          version={info.version}
          dataDir={info.dataDir}
          reinitialize={reinit}
          onDone={() => void refresh()}
        />
        {restoreDialog}
      </Stage>
    );
  }

  if (view === "locked" && status) {
    return (
      <Stage title="薪邮 · 解锁" titleRight={`v${info.version}`}>
        <LockScreen
          status={status}
          version={info.version}
          onUnlocked={() => void refresh()}
          onRestoreRequest={() => setRestoreOpen(true)}
        />
        {restoreDialog}
      </Stage>
    );
  }

  /* 已解锁：工作台（S03，数据仍是 mock —— 真实接线归 TC-200 之后） */
  return (
    <ToastProvider>
      <Workbench
        status={status}
        info={info}
        onLock={() => void refresh()}
        onRestored={() => void refresh()}
        onWiped={() => void refresh()}
      />
      {restoreDialog}
    </ToastProvider>
  );
}

/* ============================================================
   工作台占位（S03 原型结构，数据仍是写死的 mock）
   ============================================================ */

interface Batch {
  name: string;
  scope: string;
  count: number;
  result: "ok" | "warn";
  time: string;
}

const BATCHES: Batch[] = [
  { name: "2026-10 工资条", scope: "全员 / 3 组", count: 42, result: "ok", time: "11-05 09:42" },
  { name: "2026-10 补发（王五）", scope: "单人", count: 1, result: "ok", time: "11-05 10:03" },
  { name: "2026-09 工资条", scope: "全员 / 3 组", count: 41, result: "ok", time: "10-05 09:18" },
  { name: "2026-08 工资条", scope: "研发组", count: 18, result: "warn", time: "09-05 14:20" },
];

const BATCH_COLUMNS: Column<Batch>[] = [
  { key: "name", header: "批次", render: (r) => <b>{r.name}</b> },
  { key: "scope", header: "范围", render: (r) => r.scope },
  { key: "count", header: "数量", numeric: true, render: (r) => r.count },
  {
    key: "result",
    header: "结果",
    render: (r) =>
      r.result === "ok" ? <Chip tone="ok">全部成功</Chip> : <Chip tone="warn">1 失败已补发</Chip>,
  },
  { key: "time", header: "时间", render: (r) => r.time },
];

function Workbench({
  status,
  info,
  onLock,
  onRestored,
  onWiped,
}: {
  status: AdminState | null;
  info: AppInfoState;
  onLock: () => void;
  /** 恢复完数据：重新读一次状态（成员/批次可能都变了） */
  onRestored: () => void;
  /** 清空完数据：重新读一次状态 → 回到 S01 首启向导 */
  onWiped: () => void;
}) {
  const [active, setActive] = useState("overview");
  const [collapsed, setCollapsed] = useState(false);

  return (
    <AppShell
      title="薪邮"
      titleRight="无遥测 · 本机数据"
      active={active}
      onNavigate={setActive}
      collapsed={collapsed}
      onToggleCollapse={() => setCollapsed((v) => !v)}
      footer={
        <>
          {status?.email ?? "hr@company.com"}
          <br />
          已加密 · 已验证 SMTP
        </>
      }
    >
      {/* 只读横幅：常驻、不可关，解锁之后仍在 */}
      {status?.readOnly && status.readOnlyMessage && (
        <ReadOnlyBanner message={status.readOnlyMessage} />
      )}

      {active === "settings" || active === "security" ? (
        <SettingsPanel
          dataDir={info.dataDir}
          version={info.version}
          privacyNote={info.privacyNote}
          onLockNow={onLock}
          onRestored={onRestored}
          onWiped={onWiped}
        />
      ) : active === "fields" ? (
        <FieldConfig />
      ) : active === "groups" ? (
        <MemberConfig />
      ) : active === "templates" ? (
        <TemplateEditor />
      ) : active === "periods" ? (
        <PeriodPanel />
      ) : active === "export" ? (
        <ExportScreen />
      ) : active === "import" ? (
        <ImportWizard />
      ) : active === "preview" ? (
        <VerifyPanel />
      ) : active === "queue" ? (
        <QueuePanel />
      ) : active === "history" ? (
        <HistoryPanel />
      ) : active === "overview" ? (
        <Overview onGoPeriods={() => setActive("periods")} />
      ) : (
        <Card title="占位界面">
          <div className={s.placeholderText}>
            该界面对应原型章节将在对应阶段实现。
          </div>
        </Card>
      )}
    </AppShell>
  );
}

function Overview({ onGoPeriods }: { onGoPeriods?: () => void }) {
  const toast = useToast();
  return (
    <>
      <PaneHead
        title="早上好，管理员"
        sub="2026 年 11 月 9 日 · 本月发薪尚未开始"
        actions={
          <Button variant="primary" onClick={() => onGoPeriods?.()}>
            ＋ 新建月份账套
          </Button>
        }
      />

      <StatGrid>
        <Stat value={42} label="在册成员" />
        <Stat value={3} label="邮件组" />
        <Stat value={10} label="已配置工资项" tone="accent" />
        <Stat value="100%" label="上月送达率" tone="ok" />
      </StatGrid>

      <Card title="2026 年 10 月 · 发薪流程" actions={<Chip tone="ok">已归档</Chip>}>
        <Steps
          items={[
            { label: "导出模板", state: "done" },
            { label: "导入数据", state: "done" },
            { label: "预览核对", state: "done" },
            { label: "发送完成", state: "done" },
          ]}
        />
        <div className={s.placeholderText}>42 封全部送达，0 失败 · 日志已归档，可随时导出凭证</div>
      </Card>

      <Card
        title="最近发送批次"
        flush
        actions={<span className={s.placeholderText}>查看全部 →</span>}
      >
        <Table columns={BATCH_COLUMNS} rows={BATCHES} rowKey={(r) => r.name} />
      </Card>

      <Card title="本月发薪进度" tone="accent">
        <div className={s.placeholderText}>
          · 2026 年 11 月账套尚未创建，建议先导出空模板给财务填写
        </div>
        <div style={{ marginTop: 10 }}>
          <Progress value={66} />
        </div>
      </Card>

      <Card title="空状态示例">
        <Empty
          icon="▤"
          title="还没有任何月份数据"
          hint="先导出空模板，再导入财务填好的 Excel"
          action={
            <Button variant="primary" size="sm" onClick={() => onGoPeriods?.()}>
              ＋ 新建月份账套
            </Button>
          }
        />
      </Card>

      <Card title="设计系统自检">
        <div className={s.placeholderText}>
          · 设计 Token 与组件库已接入
          <br />· 上面统计块、表格、步骤条、进度条均为原型 S03 的真实结构
        </div>
        <div className={s.selfCheck}>
          <Button onClick={() => toast.push({ tone: "ok", text: "模板已导出" })}>
            触发成功 toast
          </Button>
          <Button
            onClick={() =>
              toast.push({
                tone: "danger",
                text: "1 封发送失败",
                sticky: true,
                action: {
                  label: "仅重试失败项",
                  onClick: () => toast.push({ tone: "info", text: "已入队重试" }),
                },
              })
            }
          >
            触发失败 toast（常驻）
          </Button>
        </div>
      </Card>
    </>
  );
}
