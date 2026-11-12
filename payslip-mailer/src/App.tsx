import { useCallback, useEffect, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { AppShell } from "./components/layout/AppShell";
import { Stage } from "./components/layout/Stage";
import { Button, Card, ConfirmDialog, Modal, ToastProvider } from "./components/ui";
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
import { OverviewPanel } from "./screens/OverviewPanel";
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

/** 「首启后的备份引导」已经提醒过的标记（界面偏好，不进加密库） */
const BACKUP_PROMPT_KEY = "payslip.backupPromptDismissed";

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
  /** 工作台当前在哪一屏 —— 提到 App 层，「首启后的备份引导」要能跳到设置页 */
  const [active, setActive] = useState("overview");
  /** 首启后的备份引导（TC-602 · ch.14） */
  const [backupPromptOpen, setBackupPromptOpen] = useState(false);

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

  /**
   * 首启建档完成后的备份引导（ch.14「首次启动强制引导导出加密备份」）。
   *
   * 落地口径是**引导 + 只提醒一次**，不是硬拦截 ——
   * 硬拦截会在「还没有任何数据」的时候逼人导出一份空库，那时备份没有意义；
   * 真正兜底的是设置页里**常驻**的那条提醒。
   *
   * 「提醒过了」记在 localStorage：它是界面偏好，不是业务数据，
   * 不该进加密库；它也没有外部副作用，不违反红线 5。
   */
  const maybePromptBackup = useCallback(async () => {
    try {
      if (window.localStorage.getItem(BACKUP_PROMPT_KEY)) return;
      const st = await api.getSettings();
      if (st.lastExportedAt !== null) return; // 已经备份过 → 不用打扰
      setBackupPromptOpen(true);
    } catch {
      /* 读不到就当没这回事，不打断首启 */
    }
  }, []);

  const dismissBackupPrompt = useCallback(() => {
    setBackupPromptOpen(false);
    try {
      window.localStorage.setItem(BACKUP_PROMPT_KEY, "1");
    } catch {
      /* 记不住就下次再提醒一次，不值得为它报错 */
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
          onDone={() => {
            void refresh();
            void maybePromptBackup();
          }}
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
        active={active}
        onNavigate={setActive}
        onLock={() => void refresh()}
        onRestored={() => void refresh()}
        onWiped={() => void refresh()}
      />
      <Modal
        open={backupPromptOpen}
        title="导一份加密备份？"
        onClose={dismissBackupPrompt}
        footer={
          <>
            <Button variant="ghost" onClick={dismissBackupPrompt}>
              以后再说
            </Button>
            <Button
              variant="primary"
              onClick={() => {
                setBackupPromptOpen(false);
                setActive("settings");
              }}
            >
              去导一份
            </Button>
          </>
        }
      >
        <p>
          这个应用的数据<b>只存在这台机器上</b>。一旦丢失（钥匙串条目被清理工具删掉、
          管理员口令忘记、磁盘损坏），<b>没有任何找回的办法</b>。
          导一份加密备份放在这台机器以外的地方，只要几分钟。
        </p>
      </Modal>
      {restoreDialog}
    </ToastProvider>
  );
}

function Workbench({
  status,
  info,
  active,
  onNavigate,
  onLock,
  onRestored,
  onWiped,
}: {
  status: AdminState | null;
  info: AppInfoState;
  /** 当前导航项 —— 提到 App 层是为了让「首启后的备份引导」也能跳到设置页 */
  active: string;
  onNavigate: (id: string) => void;
  onLock: () => void;
  /** 恢复完数据：重新读一次状态（成员/批次可能都变了） */
  onRestored: () => void;
  /** 清空完数据：重新读一次状态 → 回到 S01 首启向导 */
  onWiped: () => void;
}) {
  const [collapsed, setCollapsed] = useState(false);

  return (
    <AppShell
      title="薪邮"
      titleRight="无遥测 · 本机数据"
      active={active}
      onNavigate={onNavigate}
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
        <OverviewPanel
          onGoPeriods={() => onNavigate("periods")}
          onGoImport={() => onNavigate("import")}
          onGoPreview={() => onNavigate("preview")}
        />
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
