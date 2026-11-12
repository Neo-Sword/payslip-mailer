import { invoke, isTauri } from "@tauri-apps/api/core";

/* ============================================================
   IPC 边界：前端拿数据的唯一入口（红线 5）
   所有外部副作用都在 Rust 的 Command 层里完成，
   前端不直接碰文件系统、不直接连 SMTP、不长期持有工资数值或凭据。
   ============================================================ */

/** Rust 侧 `error.rs` 的错误码全集（对外契约，改动要同步那份词表） */
export type ErrorCode =
  | "NOT_INITIALIZED"
  | "BAD_CREDENTIALS"
  | "LOCKED"
  | "INVALID_EMAIL"
  | "DUPLICATE"
  | "NOT_FOUND"
  | "TEMPLATE_ERROR"
  | "VALIDATION_ERROR"
  | "SMTP_AUTH_FAILED"
  | "SMTP_TRANSIENT"
  | "SMTP_INVALID_RECIPIENT"
  | "DATABASE"
  | "MIGRATION_DRIFT"
  | "READ_ONLY"
  | "KEYCHAIN"
  | "KEYCHAIN_MISSING"
  | "IO"
  | "INTERNAL"
  | "BACKUP_PASSWORD_WRONG"
  | "BACKUP_CORRUPTED"
  | "BACKUP_VERSION_TOO_NEW"
  | "BACKUP_FAILED"
  | "WIPE_PASSWORD_WRONG";

/** 后端错误：带稳定 code，前端据此分支（而不是靠文案字符串匹配） */
export class BackendError extends Error {
  readonly code: ErrorCode;

  constructor(code: ErrorCode, message: string) {
    super(message);
    this.name = "BackendError";
    this.code = code;
  }
}

/**
 * 把 Tauri 回传的错误还原成 `BackendError`。
 *
 * Rust 侧 `AppError` 实现了 `Serialize`，序列化成 `{code, message}`；
 * Tauri v2 把这个结构体作为**一个 JSON 字符串**回传给前端（不是对象），
 * 所以这里先按字符串试解析，失败再退回通用码。
 */
function toBackendError(raw: unknown): BackendError {
  if (typeof raw === "string") {
    try {
      const parsed = JSON.parse(raw) as { code?: string; message?: string };
      if (parsed && typeof parsed.code === "string") {
        return new BackendError(parsed.code as ErrorCode, parsed.message ?? "操作未完成");
      }
    } catch {
      /* 不是 JSON：按原文当消息处理 */
    }
    return new BackendError("INTERNAL", raw);
  }
  if (raw && typeof raw === "object") {
    const obj = raw as { code?: string; message?: string };
    if (typeof obj.code === "string") {
      return new BackendError(obj.code as ErrorCode, obj.message ?? "操作未完成");
    }
  }
  return new BackendError("INTERNAL", "操作未完成，请重试。");
}

async function call<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  try {
    return await invoke<T>(cmd, args);
  } catch (raw) {
    throw toBackendError(raw);
  }
}

/* ---------------- 后端返回的数据形状 ---------------- */

/** `infra::admins::AdminState`（serde camelCase） */
/** 「把旧库移到一边」的结果：旧库去了哪（没移走时是 null） */
export interface ArchiveReport {
  previousPath: string;
  archivedPath: string | null;
}

export interface AdminState {
  initialized: boolean;
  email: string | null;
  /** 库开着 = 能进工作台（ADR-0012 §7） */
  unlocked: boolean;
  /** 口令失败锁定（与 unlocked 是两个「锁」） */
  locked: boolean;
  secondsLeft: number;
  remainingAttempts: number;
  readOnly: boolean;
  /** 只读横幅整句文案；可写态为 null */
  readOnlyMessage: string | null;
}

/** `commands::auth::UnlockResult` */
export interface UnlockResult {
  rehashed: boolean;
  readOnly: boolean;
  unlocked: boolean;
}

/** `commands::auth::SettingsState` */
export interface SettingsState {
  idleLockMinutes: number;
  orgName: string | null;
  /** 上次从备份恢复的时刻（epoch 秒）；没恢复过是 null */
  lastRestoredAt: number | null;
  /** 上次导出备份的时刻（epoch 秒）；没导出过是 null —— 常驻备份提醒靠它说话 */
  lastExportedAt: number | null;
}

/** 备份文件在界面上的样子（`infra::backup::BackupFileInfo`） */
export interface BackupFileInfo {
  path: string;
  /** 文件名。只有日期 —— 不含姓名/组名/金额（红线 2） */
  name: string;
  size: number;
  /** 创建时刻（epoch 秒），从清单里读 */
  createdAt: number;
  kind: "manual" | "preRestoreSafety";
  appVersion: string;
  schemaVersion: number;
}

/** 一个工资条项目（`infra::fields::Field`） */
export interface FieldRow {
  id: number;
  key: string;
  label: string;
  dataType: "text" | "number" | "money" | "percent" | "formula";
  sortOrder: number;
  required: boolean;
  /** false = 已停用（不进新表头、不在新建模板的白名单里） */
  visible: boolean;
  formula: string | null;
  align: string;
}

/** 一个邮件模板（`infra::templates::Template`） */
export interface TemplateRow {
  id: number;
  name: string;
  /** 主题的源码（可含变量） */
  subjectTpl: string;
  /** 正文 HTML 的源码（可含变量） */
  bodyHtml: string;
  /** 纯文本兜底版本（自动生成） */
  bodyText: string;
  isDefault: boolean;
}

/** 变量面板里的一项 */
export interface VariableInfo {
  /** 模板里写什么（如 `姓名`、`字段.base_salary`） */
  name: string;
  /** 界面上显示什么 */
  label: string;
  /** 为空时是否要拦下 */
  required: boolean;
  kind: "builtin" | "field";
}

/** 模板变量校验的结果 */
export interface VariableCheck {
  /** 未解析的变量（不能设为默认） */
  unknown: { variable: string; message: string }[];
  /** 必填但为空的变量（发送前拦下这个人） */
  empty: { variable: string; message: string }[];
}

/** 渲染结果（预览用） */
export interface Rendered {
  subject: string;
  html: string;
  text: string;
}

/** 工资表的一列（`domain::sheet::SheetColumn`） */
export interface SheetColumn {
  title: string;
  key: string;
  kind: "fixed" | "field";
  required: boolean;
  isFormula: boolean;
  numeric: boolean;
  dataType: string | null;
}

/** 工资表的结构（`domain::sheet::SheetSchema`） */
export interface SheetSchema {
  columns: SheetColumn[];
}

/** 导出结果 */
export interface ExportTemplateResult {
  path: string;
  columns: SheetColumn[];
  rowCount: number;
}

/** 一列映射到哪儿（`domain::import::ColumnTarget`） */
export type ColumnTarget =
  | { kind: "fixed"; key: string }
  | { kind: "field"; key: string }
  | { kind: "skip" };

/** 表头映射的可信度（`domain::import::MatchKind`） */
export type MatchKind = "exact" | "fuzzy" | "unmatched";

/** 一列的映射结果（`domain::import::ColumnMapping`） */
export interface ColumnMapping {
  index: number;
  header: string;
  target: ColumnTarget;
  matchKind: MatchKind;
  needsConfirmation: boolean;
}

/** 一行上的一个问题（`domain::import::RowProblem`） */
export interface RowProblem {
  /** Excel 里的行号（1 基，含表头行） */
  row: number;
  column: string;
  severity: "error" | "warning";
  message: string;
}

/** 导入的统计（`domain::import::ImportSummary`） */
export interface ImportSummary {
  success: number;
  warnings: number;
  errors: number;
}

/** 导入预览（`commands::import::ImportPreview`） */
export interface ImportPreview {
  fileName: string;
  mappings: ColumnMapping[];
  /** 固定列缺了哪些（空 = 都对上了） */
  missingFixed: string[];
  needsConfirmation: boolean;
  summary: ImportSummary;
  problems: RowProblem[];
  rowCount: number;
}

/** 导入结果（`commands::import::ImportOutcome`） */
export interface ImportOutcome {
  written: number;
  replaced: number;
  cancelled: boolean;
  summary: ImportSummary;
  warnings: number;
  /** 「身份待核对」的行数（这些行不会进入发送范围） */
  identityUnverified: number;
}

/** 冲突策略（`domain::import::ConflictStrategy`） */
export type ConflictStrategy = "overwrite" | "fillEmpty" | "cancel";

/** S07 表格里的一行（`infra::records::RecordRow`） */
export interface RecordRow {
  memberId: number;
  name: string;
  email: string;
  employeeNo: string | null;
  /** 字段 key → 值（含公式列）。键不存在 = 这一项没有 */
  values: Record<string, string>;
  /** 校验列：空 = 通过；否则是问题的说明 */
  validation: string;
  hasProblem: boolean;
  source: string;
}

/** 整月合计（`infra::records::PeriodTotals`） */
export interface PeriodTotals {
  earnings: number;
  deductions: number;
  net: number;
}

/** 单人预览（`infra::verify::PreviewPayload`） */
export interface PreviewPayload {
  subject: string;
  html: string;
  text: string;
  /** 渲染时这一行的内容 checksum（内容一致的「预览侧」值） */
  checksum: string;
}

/** 一个成员的核对结果（`infra::verify::MemberVerification`） */
export interface MemberVerification {
  memberId: number;
  name: string;
  email: string;
  blockedReasons: string[];
  variableProblems: string[];
  sendable: boolean;
  preview: PreviewPayload | null;
}

/** 发送范围的核对报告（`infra::verify::ScopeVerification`） */
export interface ScopeVerification {
  ok: boolean;
  total: number;
  sendableCount: number;
  blocked: MemberVerification[];
  members: MemberVerification[];
}

/** 一个发送批次（`infra::batch::Batch`） */
export interface BatchRow {
  id: number;
  periodId: number;
  /** running / paused / aborted / finished */
  status: string;
  ratePerMin: number;
  total: number;
  success: number;
  failed: number;
  startedAt: number | null;
  finishedAt: number | null;
}

/** 发信配置（`infra::mailer::SmtpSettings`）—— 密码不在其中，只在 Keychain */
export interface SmtpSettings {
  host: string;
  port: number;
  /** ssl / starttls / none */
  security: string;
  username: string;
  fromName: string;
  fromEmail: string;
  replyTo: string | null;
  ratePerMin: number;
  /** Keychain 条目名 —— 不含密码本体 */
  credentialRef: string;
}

/** 发送历史的一条批次（`infra::sendlog::HistoryEntry`） */
export interface HistoryEntry {
  batch: BatchRow;
  progress: BatchProgress;
}

/** 逐封明细（`infra::sendlog::JobDetail`） */
export interface JobDetail {
  jobId: number;
  memberId: number;
  name: string;
  toEmail: string;
  status: string;
  attempts: number;
  smtpCode: number | null;
  errorCode: string | null;
  sentAt: number | null;
}

/** 测试连接的结果（`infra::mailer::TestResult`） */
export type TestResult =
  | { kind: "ok" }
  | { kind: "authFailed" }
  | { kind: "failed"; detail: string };

/** 批次进度（`infra::batch::BatchProgress`）—— 「已发 27/60」从库算 */
export interface BatchProgress {
  total: number;
  success: number;
  failed: number;
  /** queued（含还没到点的 retrying） */
  pending: number;
  /** 正在发或存疑的（sending） */
  inFlight: number;
}

export interface BatchWithProgress {
  batch: BatchRow;
  progress: BatchProgress;
}

/** 一个月份账套（`infra::periods::Period`） */
export interface PeriodRow {
  id: number;
  /** `YYYY-MM` */
  yearMonth: string;
  status: "draft" | "ready" | "sending" | "sent" | "archived";
  fieldSnapshot: string | null;
  lockedAt: number | null;
  /** 现在能不能编辑工资数值 */
  editable: boolean;
  readOnly: boolean;
}

/** 快照里的一个字段（冻结当时的定义） */
export interface SnapshotField {
  key: string;
  label: string;
  dataType: string;
  /** 公式源码（不是算出来的值） */
  formula: string | null;
  required: boolean;
  visible: boolean;
  sortOrder: number;
}

/** 一个成员（`infra::members::Member`） */
export interface MemberRow {
  id: number;
  name: string;
  email: string;
  employeeNo: string | null;
  /** false = 停用（不在发送范围，但记录还在） */
  active: boolean;
  note: string | null;
  /** 所属的组（名字）。一个人可以属于多个组。 */
  groups: string[];
}

/** 一个邮件组（`infra::members::Group`） */
export interface GroupRow {
  id: number;
  name: string;
  note: string | null;
  sortOrder: number;
  /** 组里启用的成员数 */
  activeMemberCount: number;
  /** 组里全部成员数（含停用的） */
  memberCount: number;
}

/** 批量粘贴的预览（`domain::member::PastePreview`） */
export interface PastePreviewData {
  accepted: { name: string; email: string }[];
  problems: { line: number; field: string; message: string }[];
  deduped: number;
  skippedHeader: boolean;
  delimiter: string;
}

/** 销毁前的清单（`infra::wipe::WipePreview`） */
export interface WipePreview {
  databasePath: string;
  databaseSize: number;
  /** 本机备份文件的名字 */
  backupNames: string[];
  logsPath: string;
}

/** 销毁做完了的结果（`infra::wipe::WipeReport`） */
export interface WipeReport {
  databaseRemoved: boolean;
  backupsRemoved: number;
  keychainEntryRemoved: boolean;
  logsKept: boolean;
}

/** 恢复做完了的结果（`infra::backup::RestoreOutcome`） */
export interface RestoreOutcome {
  schemaVersion: number;
  /** 恢复前自动留的保险备份路径 */
  safetyBackup: string | null;
  /** 备份里那时的管理员邮箱 —— 解锁要用备份里那时的口令 */
  adminEmail: string | null;
}

/** `commands::app::AppInfo`（该结构体没开 camelCase，字段是下划线） */
export interface AppInfo {
  name: string;
  version: string;
  privacy_note: string;
  data_dir: string;
}

/* ---------------- Command ---------------- */

export const api = {
  adminStatus: (): Promise<AdminState> => call<AdminState>("admin_status"),
  unlock: (password: string, rememberDevice: boolean): Promise<UnlockResult> =>
    call<UnlockResult>("unlock", { password, rememberDevice }),
  createAdmin: (email: string, password: string, orgName: string): Promise<void> =>
    call<void>("create_admin", { email, password, orgName }),
  getSettings: async (): Promise<SettingsState> => {
    if (!previewMode) return await call<SettingsState>("get_settings");
    // 预览模式：备份提醒那一段要有个能看见的样子（TC-602）。
    return {
      idleLockMinutes: 10,
      orgName: "某某科技有限公司",
      lastRestoredAt: null,
      lastExportedAt: 1790000000,
    };
  },
  appInfo: (): Promise<AppInfo> => call<AppInfo>("app_info"),
  lockNow: (): Promise<void> => call<void>("lock_now"),
  changePassword: (oldPassword: string, newPassword: string): Promise<void> =>
    call<void>("change_password", { oldPassword, newPassword }),
  setIdleLockMinutes: (minutes: number): Promise<void> =>
    call<void>("set_idle_lock_minutes", { minutes }),
  /** 把读不出来的旧库移到一边（不删），以便重新初始化 */
  reinitializeLocalState: (): Promise<ArchiveReport> =>
    call<ArchiveReport>("reinitialize_local_state"),
  exportBackup: (password: string): Promise<BackupFileInfo> =>
    call<BackupFileInfo>("export_backup", { password }),
  listBackups: (): Promise<BackupFileInfo[]> => call<BackupFileInfo[]>("list_backups"),
  restoreBackup: (path: string, password: string): Promise<RestoreOutcome> =>
    call<RestoreOutcome>("restore_backup", { path, password }),
  wipePreview: (): Promise<WipePreview> => call<WipePreview>("wipe_preview"),
  wipeData: (password: string): Promise<WipeReport> =>
    call<WipeReport>("wipe_data", { password }),
  listFields: async (): Promise<FieldRow[]> =>
    previewMode ? PREVIEW_FIELDS : await call<FieldRow[]>("list_fields"),
  createField: (
    key: string,
    label: string,
    dataType: string,
    required: boolean,
    formula: string | null,
  ): Promise<FieldRow> =>
    call<FieldRow>("create_field", { key, label, dataType, required, formula }),
  updateField: (
    id: number,
    label: string,
    dataType: string,
    required: boolean,
    formula: string | null,
  ): Promise<void> =>
    call<void>("update_field", { id, label, dataType, required, formula }),
  setFieldVisible: (id: number, visible: boolean): Promise<void> =>
    call<void>("set_field_visible", { id, visible }),
  reorderFields: (order: number[]): Promise<void> =>
    call<void>("reorder_fields", { order }),
  previewHeader: async (): Promise<string[]> =>
    previewMode
      ? PREVIEW_FIELDS.filter((f) => f.visible).map((f) => f.label)
      : await call<string[]>("preview_header"),
  listSelectableVariables: (): Promise<[string, string][]> =>
    call<[string, string][]>("list_selectable_variables"),
  listMembers: async (): Promise<MemberRow[]> =>
    previewMode ? PREVIEW_MEMBERS : await call<MemberRow[]>("list_members"),
  listSendableMembers: (): Promise<MemberRow[]> =>
    call<MemberRow[]>("list_sendable_members"),
  listGroups: async (): Promise<GroupRow[]> =>
    previewMode ? PREVIEW_GROUPS : await call<GroupRow[]>("list_groups"),
  memberCount: async (): Promise<number> =>
    previewMode ? PREVIEW_MEMBERS.length : await call<number>("member_count"),
  createMember: (
    name: string,
    email: string,
    employeeNo: string | null,
    groupIds: number[],
  ): Promise<MemberRow> =>
    call<MemberRow>("create_member", { name, email, employeeNo, groupIds }),
  updateMember: (
    id: number,
    name: string,
    email: string,
    employeeNo: string | null,
    groupIds: number[],
  ): Promise<void> =>
    call<void>("update_member", { id, name, email, employeeNo, groupIds }),
  setMemberActive: (id: number, active: boolean): Promise<void> =>
    call<void>("set_member_active", { id, active }),
  createGroup: (name: string, note: string | null): Promise<GroupRow> =>
    call<GroupRow>("create_group", { name, note }),
  deleteGroup: (id: number): Promise<void> => call<void>("delete_group", { id }),
  previewMemberPaste: (text: string): Promise<PastePreviewData> =>
    call<PastePreviewData>("preview_member_paste", { text }),
  pasteMembers: (drafts: { name: string; email: string }[]): Promise<number> =>
    call<number>("paste_members", { drafts }),
  listTemplates: async (): Promise<TemplateRow[]> =>
    previewMode ? PREVIEW_TEMPLATES : await call<TemplateRow[]>("list_templates"),
  templateVariables: async (): Promise<VariableInfo[]> =>
    previewMode ? PREVIEW_VARIABLES : await call<VariableInfo[]>("template_variables"),
  createTemplate: (name: string, subject: string, body: string): Promise<TemplateRow> =>
    call<TemplateRow>("create_template", { name, subject, body }),
  updateTemplate: (
    id: number,
    name: string,
    subject: string,
    body: string,
  ): Promise<void> => call<void>("update_template", { id, name, subject, body }),
  duplicateTemplate: (id: number): Promise<TemplateRow> =>
    call<TemplateRow>("duplicate_template", { id }),
  setDefaultTemplate: (id: number): Promise<void> =>
    call<void>("set_default_template", { id }),
  deleteTemplate: (id: number): Promise<void> => call<void>("delete_template", { id }),
  previewTemplate: async (subject: string, body: string): Promise<Rendered> => {
    if (previewMode) {
      const filled = body
        .replace(/\{\{姓名\}\}/g, "张三")
        .replace(/\{\{年月\}\}/g, "2026 年 10 月")
        .replace(/\{\{实发工资\}\}/g, "16,840.00")
        .replace(/\{\{工资条表格\}\}/g, "<table><tr><td>基本工资</td><td>18,000.00</td></tr></table>");
      return {
        subject: subject.replace(/\{\{年月\}\}/g, "2026 年 10 月"),
        html: filled,
        text: filled.replace(/<[^>]+>/g, " "),
      };
    }
    return await call<Rendered>("preview_template", { subject, body });
  },
  evaluateFormula: async (
    formula: string,
    values: Record<string, string>,
    totalEarnings = 0,
    totalDeductions = 0,
  ): Promise<string> => {
    if (previewMode) {
      // **预览模式不试算**。
      // 浏览器里再写一份求值器，就是 ADR-0018 反对的「第二份规则」——
      // 两份规则迟早给出不同的错误信息，而使用者分不清看到的是哪一份。
      // 真机的行为由 Rust 侧 `domain::formula` 负责，那里有测试钉着。
      throw new BackendError(
        "VALIDATION_ERROR",
        "浏览器预览模式不试算 —— 真机上才试算（真机的求值器在 Rust 侧，有单测钉着）",
      );
    }
    return await call<string>("evaluate_formula", {
      formula,
      values,
      totalEarnings,
      totalDeductions,
    });
  },
  previewTemplateHeader: async (periodId: number): Promise<SheetSchema> => {
    if (!previewMode) return await call<SheetSchema>("preview_template_header", { periodId });
    // 预览模式：表头 = 3 个固定列 + 预览字段集（与后端 ADR-0020 的口径一致）。
    // 没有这一段，浏览器预览里这一屏永远是「0 列 + 报错」，调样式根本无从下手。
    return { columns: PREVIEW_SHEET_COLUMNS };
  },
  exportTemplate: (
    periodId: number,
    includeInstructions: boolean,
    includeSampleRows: boolean,
    includeMemberList: boolean,
  ): Promise<ExportTemplateResult> =>
    call<ExportTemplateResult>("export_template", {
      periodId,
      includeInstructions,
      includeSampleRows,
      includeMemberList,
    }),
  listPeriods: async (): Promise<PeriodRow[]> =>
    previewMode ? PREVIEW_PERIODS : await call<PeriodRow[]>("list_periods"),
  getPeriod: (id: number): Promise<PeriodRow> => call<PeriodRow>("get_period", { id }),
  createPeriod: (yearMonth: string): Promise<PeriodRow> =>
    call<PeriodRow>("create_period", { yearMonth }),
  transitionPeriod: (id: number, target: string): Promise<PeriodRow> =>
    call<PeriodRow>("transition_period", { id, target }),
  unlockPeriod: (id: number): Promise<PeriodRow> => call<PeriodRow>("unlock_period", { id }),
  getPeriodFields: async (id: number): Promise<SnapshotField[]> => {
    if (!previewMode) return await call<SnapshotField[]>("get_period_fields", { id });
    const p = PREVIEW_PERIODS.find((x) => x.id === id);
    if (!p || p.status === "draft") {
      return [
        { key: "base_salary", label: "基本工资", dataType: "money", formula: null, required: true, visible: true, sortOrder: 0 },
        { key: "performance", label: "绩效奖金", dataType: "money", formula: null, required: false, visible: true, sortOrder: 1 },
        { key: "net_pay", label: "实发工资", dataType: "formula", formula: "应发合计 + 扣款合计", required: true, visible: true, sortOrder: 2 },
      ];
    }
    return [
      { key: "base_salary", label: "基本工资（快照）", dataType: "money", formula: null, required: true, visible: true, sortOrder: 0 },
      { key: "net_pay", label: "实发工资（快照）", dataType: "formula", formula: "应发合计 + 扣款合计", required: true, visible: true, sortOrder: 1 },
    ];
  },
  previewImport: (periodId: number, path: string): Promise<ImportPreview> =>
    call<ImportPreview>("preview_import", { periodId, path }),
  commitImport: (
    periodId: number,
    path: string,
    mappings: ColumnMapping[],
    strategy: ConflictStrategy | null,
  ): Promise<ImportOutcome> =>
    call<ImportOutcome>("commit_import", {
      periodId,
      path,
      mappings: mappings.map((m) => ({ index: m.index, target: m.target })),
      strategy,
    }),
  exportImportErrors: (
    periodId: number,
    path: string,
    mappings: ColumnMapping[],
  ): Promise<string> =>
    call<string>("export_import_errors", {
      periodId,
      path,
      mappings: mappings.map((m) => ({ index: m.index, target: m.target })),
    }),
  listRecords: async (periodId: number): Promise<RecordRow[]> => {
    if (!previewMode) return await call<RecordRow[]>("list_records", { periodId });
    return PREVIEW_RECORDS;
  },
  updateRecordCell: (
    periodId: number,
    memberId: number,
    key: string,
    value: string,
  ): Promise<RecordRow> =>
    call<RecordRow>("update_record_cell", { periodId, memberId, key, value }),
  deleteRecord: (periodId: number, memberId: number): Promise<void> =>
    call<void>("delete_record", { periodId, memberId }),
  getPeriodTotals: async (periodId: number): Promise<PeriodTotals> => {
    if (!previewMode) return await call<PeriodTotals>("get_period_totals", { periodId });
    return { earnings: 18500, deductions: 2350, net: 16150 };
  },
  verifySendScope: (
    periodId: number,
    memberIds: number[],
    templateId: number | null,
  ): Promise<ScopeVerification> =>
    call<ScopeVerification>("verify_send_scope", {
      periodId,
      memberIds,
      templateId,
    }),
  renderMemberPayslip: (
    periodId: number,
    memberId: number,
    templateId: number | null,
  ): Promise<PreviewPayload> =>
    call<PreviewPayload>("render_member_payslip", { periodId, memberId, templateId }),
  startBatch: (
    periodId: number,
    memberIds: number[],
    templateId: number | null,
  ): Promise<BatchRow> => call<BatchRow>("start_batch", { periodId, memberIds, templateId }),
  findUnfinishedBatch: (): Promise<BatchWithProgress | null> =>
    call<BatchWithProgress | null>("find_unfinished_batch"),
  getBatchProgress: (batchId: number): Promise<BatchProgress> =>
    call<BatchProgress>("get_batch_progress", { batchId }),
  recoverAndRunBatch: (batchId: number): Promise<BatchRow> =>
    call<BatchRow>("recover_and_run_batch", { batchId }),
  retryFailedJobs: (batchId: number): Promise<number> =>
    call<number>("retry_failed_jobs", { batchId }),
  getSmtpSettings: (): Promise<SmtpSettings | null> =>
    call<SmtpSettings | null>("get_smtp_settings"),
  saveSmtpSettings: (settings: SmtpSettings, password: string): Promise<SmtpSettings> =>
    call<SmtpSettings>("save_smtp_settings", { settings, password }),
  testSmtpConnection: (settings: SmtpSettings, password: string): Promise<TestResult> =>
    call<TestResult>("test_smtp_connection", { settings, password }),
  listSendHistory: async (): Promise<HistoryEntry[]> => {
    if (!previewMode) return await call<HistoryEntry[]>("list_send_history");
    return PREVIEW_BATCHES.map((b) => ({
      batch: b,
      progress: {
        total: b.total,
        success: b.success,
        failed: b.failed,
        pending: Math.max(0, b.total - b.success - b.failed),
        inFlight: 0,
      },
    }));
  },
  /** 发送队列用：列出全部批次（含进度统计） */
  listSendBatches: async (): Promise<BatchRow[]> => {
    if (!previewMode) return await call<BatchRow[]>("list_send_batches");
    return PREVIEW_BATCHES;
  },
  pauseSendBatch: (batchId: number): Promise<void> =>
    call<void>("pause_send_batch", { batchId }),
  resumeSendBatch: (batchId: number): Promise<void> =>
    call<void>("resume_send_batch", { batchId }),
  abortSendBatch: (batchId: number): Promise<void> =>
    call<void>("abort_send_batch", { batchId }),
  getBatchJobs: (batchId: number): Promise<JobDetail[]> =>
    call<JobDetail[]>("get_batch_jobs", { batchId }),
  exportBatchVoucher: (batchId: number): Promise<string> =>
    call<string>("export_batch_voucher", { batchId }),
  checkTemplateVariables: async (
    subject: string,
    body: string,
  ): Promise<VariableCheck> => {
    if (previewMode) {
      const used = [...`${subject} ${body}`.matchAll(/\{\{([^}]+)\}\}/g)].map((m) => m[1]);
      const known = new Set(PREVIEW_VARIABLES.map((v) => v.name));
      const unknown = used
        .filter((u) => !known.has(u))
        .map((u) => ({ variable: u, message: `未解析的变量：${u}` }));
      return { unknown, empty: [] };
    }
    return await call<VariableCheck>("check_template_variables", { subject, body });
  },
};

/* ============================================================
   浏览器预览模式
   `npm run dev` 会在浏览器里打开，那里没有 Tauri 后端，invoke 必然失败。
   为了能在浏览器里调这几屏的样式，这里给一个**只在浏览器里生效**的假状态：
   桌面应用（isTauri() === true）永远走真实 Command，这段代码进不了生产路径。
   ============================================================ */

export const isDesktop = isTauri();

/** 仅浏览器预览用：?preview=setup | locked | unlocked | readonly | keychain */
const previewMode = (() => {
  if (isDesktop) return null;
  const q = new URLSearchParams(window.location.search).get("preview");
  const allowed = ["setup", "locked", "unlocked", "readonly", "keychain"];
  return allowed.includes(q ?? "") ? q : "setup";
})();

export const isPreview = previewMode !== null;

/** 只读横幅的文案：与后端 `infra::db::read_only_message` 同源（这里照抄一份供预览） */
const PREVIEW_READ_ONLY_MESSAGE =
  "数据库处于只读状态（迁移未完成），本次修改未写入。原因：第 2 版迁移与本机数据记录的不一致。\
本机数据仍可正常查看；请不要手工修改数据文件，可从备份恢复或联系维护人员。";

/** 预览模式下的假字段表 —— 与后端的默认字段集一致（设计文档 S04 原型那组）。 */
const PREVIEW_FIELDS: FieldRow[] = [
  { id: 1, key: "base_salary", label: "基本工资", dataType: "money", sortOrder: 0, required: true, visible: true, formula: null, align: "right" },
  { id: 2, key: "performance", label: "绩效奖金", dataType: "money", sortOrder: 1, required: false, visible: true, formula: null, align: "right" },
  { id: 3, key: "meal_allowance", label: "餐补", dataType: "money", sortOrder: 2, required: false, visible: true, formula: null, align: "right" },
  { id: 4, key: "social_insurance", label: "社保个人", dataType: "money", sortOrder: 3, required: false, visible: true, formula: null, align: "right" },
  { id: 5, key: "housing_fund", label: "公积金", dataType: "money", sortOrder: 4, required: false, visible: true, formula: null, align: "right" },
  { id: 6, key: "net_pay", label: "实发工资", dataType: "formula", sortOrder: 5, required: true, visible: true, formula: "base_salary + performance + meal_allowance + social_insurance + housing_fund", align: "right" },
  { id: 7, key: "remark", label: "备注", dataType: "text", sortOrder: 6, required: false, visible: false, formula: null, align: "left" },
];

/** 预览模式下的假名单与组（与后端的默认示例一致）。 */
const PREVIEW_MEMBERS: MemberRow[] = [
  { id: 1, name: "张三", email: "zhangsan@company.com", employeeNo: "E001", active: true, note: null, groups: ["研发组"] },
  { id: 2, name: "李四", email: "lisi@company.com", employeeNo: "E002", active: true, note: null, groups: ["研发组"] },
  { id: 3, name: "王五", email: "wangwu@company.com", employeeNo: "E003", active: true, note: null, groups: ["市场组", "全员"] },
  { id: 4, name: "赵六", email: "zhaoliu@company.com", employeeNo: "E004", active: false, note: null, groups: ["职能组"] },
];

const PREVIEW_GROUPS: GroupRow[] = [
  { id: 1, name: "全员", note: null, sortOrder: 0, activeMemberCount: 1, memberCount: 1 },
  { id: 2, name: "研发组", note: null, sortOrder: 1, activeMemberCount: 2, memberCount: 2 },
  { id: 3, name: "市场组", note: null, sortOrder: 2, activeMemberCount: 1, memberCount: 1 },
  { id: 4, name: "职能组", note: null, sortOrder: 3, activeMemberCount: 0, memberCount: 1 },
];

/** 预览模式下的假模板（与后端的默认模板一致：设计文档 S10 的「标准中文模板」）。 */
const PREVIEW_TEMPLATES: TemplateRow[] = [
  {
    id: 1,
    name: "标准中文模板",
    subjectTpl: "{{年月}} 工资条",
    bodyHtml:
      "<p>{{姓名}} 您好：</p>\n<p>以下是您 <b>{{年月}}</b> 的工资条，请查收并妥善保管。</p>\n{{工资条表格}}\n<p>本月实发：<b>{{实发工资}}</b></p>",
    bodyText: "张三 您好：\n以下是您 2026 年 10 月 的工资条，请查收并妥善保管。",
    isDefault: true,
  },
  {
    id: 2,
    name: "简短版",
    subjectTpl: "{{年月}} 工资条",
    bodyHtml: "<p>{{姓名}}，您的 {{年月}} 工资条已生成，实发 {{实发工资}}。</p>",
    bodyText: "张三，您的 2026 年 10 月 工资条已生成。",
    isDefault: false,
  },
];

/** 预览模式下的变量面板（内置变量 + 未停用的字段）。 */
const PREVIEW_VARIABLES: VariableInfo[] = [
  { name: "姓名", label: "姓名", required: true, kind: "builtin" },
  { name: "工号", label: "工号", required: false, kind: "builtin" },
  { name: "年月", label: "年月", required: true, kind: "builtin" },
  { name: "公司名称", label: "公司名称", required: true, kind: "builtin" },
  { name: "组名", label: "组名", required: false, kind: "builtin" },
  { name: "回信邮箱", label: "回信邮箱", required: false, kind: "builtin" },
  { name: "应发合计", label: "应发合计", required: true, kind: "builtin" },
  { name: "扣款合计", label: "扣款合计", required: true, kind: "builtin" },
  { name: "实发工资", label: "实发工资", required: true, kind: "builtin" },
  { name: "工资条表格", label: "工资条表格", required: true, kind: "builtin" },
  { name: "字段.base_salary", label: "基本工资", required: true, kind: "field" },
  { name: "字段.performance", label: "绩效奖金", required: false, kind: "field" },
  { name: "字段.social_insurance", label: "社保个人", required: false, kind: "field" },
  { name: "字段.net_pay", label: "实发工资", required: true, kind: "field" },
];

/** 预览模式下的假账套。 */
const PREVIEW_PERIODS: PeriodRow[] = [
  { id: 1, yearMonth: "2026-10", status: "draft", fieldSnapshot: null, lockedAt: null, editable: true, readOnly: false },
  { id: 2, yearMonth: "2026-09", status: "sent", fieldSnapshot: "[]", lockedAt: 1790000000, editable: false, readOnly: true },
];

/** 预览模式下的假工资记录（概览屏算「已导入行数」用）。 */
const PREVIEW_RECORDS: RecordRow[] = [
  {
    memberId: 1,
    name: "张三",
    email: "zhangsan@company.com",
    employeeNo: "E001",
    values: { base_salary: "18000", performance: "2000", meal_allowance: "300", social_insurance: "1500", housing_fund: "1000", net_pay: "17800" },
    validation: "",
    hasProblem: false,
    source: "import",
  },
  {
    memberId: 2,
    name: "李四",
    email: "lisi@company.com",
    employeeNo: "E002",
    values: { base_salary: "15000", performance: "1000", meal_allowance: "300", social_insurance: "1200", housing_fund: "800", net_pay: "14300" },
    validation: "",
    hasProblem: false,
    source: "import",
  },
];

/** 预览模式下的假批次（发送队列那屏）。 */
const PREVIEW_BATCHES: BatchRow[] = [
  { id: 7, periodId: 1, status: "running", ratePerMin: 30, total: 42, success: 27, failed: 1, startedAt: 1790000000, finishedAt: null },
  { id: 6, periodId: 2, status: "finished", ratePerMin: 30, total: 41, success: 41, failed: 0, startedAt: 1789000000, finishedAt: 1789000068 },
];

/**
 * 预览模式下的空模板表头：**3 个固定列 + 预览字段集**。 *
 * 口径与后端一致（ADR-0020）：固定列在前（姓名 / 邮箱 / 工号），
 * 它们是导入映射的锚点；公式列（实发工资）标 `isFormula`，
 * 导出的文件里那一格写「公式自动计算」而不是数字。
 */
const PREVIEW_SHEET_COLUMNS: SheetColumn[] = [
  { title: "姓名", key: "name", kind: "fixed", required: true, isFormula: false, numeric: false, dataType: "text" },
  { title: "邮箱", key: "email", kind: "fixed", required: true, isFormula: false, numeric: false, dataType: "text" },
  { title: "工号", key: "employee_no", kind: "fixed", required: false, isFormula: false, numeric: false, dataType: "text" },
  { title: "基本工资", key: "base_salary", kind: "field", required: true, isFormula: false, numeric: true, dataType: "money" },
  { title: "绩效奖金", key: "performance", kind: "field", required: false, isFormula: false, numeric: true, dataType: "money" },
  { title: "餐补", key: "meal_allowance", kind: "field", required: false, isFormula: false, numeric: true, dataType: "money" },
  { title: "社保个人", key: "social_insurance", kind: "field", required: false, isFormula: false, numeric: true, dataType: "money" },
  { title: "公积金", key: "housing_fund", kind: "field", required: false, isFormula: false, numeric: true, dataType: "money" },
  { title: "实发工资", key: "net_pay", kind: "field", required: true, isFormula: true, numeric: true, dataType: "formula" },
  { title: "备注", key: "remark", kind: "field", required: false, isFormula: false, numeric: false, dataType: "text" },
];

const PREVIEW_STATUS: Record<string, AdminState> = {
  setup: {
    initialized: false,
    email: null,
    unlocked: false,
    locked: false,
    secondsLeft: 0,
    remainingAttempts: 5,
    readOnly: false,
    readOnlyMessage: null,
  },
  locked: {
    initialized: true,
    email: "hr@company.com",
    unlocked: false,
    locked: false,
    secondsLeft: 0,
    remainingAttempts: 5,
    readOnly: false,
    readOnlyMessage: null,
  },
  unlocked: {
    initialized: true,
    email: "hr@company.com",
    unlocked: true,
    locked: false,
    secondsLeft: 0,
    remainingAttempts: 5,
    readOnly: false,
    readOnlyMessage: null,
  },
  readonly: {
    initialized: true,
    email: "hr@company.com",
    unlocked: true,
    locked: false,
    secondsLeft: 0,
    remainingAttempts: 5,
    readOnly: true,
    readOnlyMessage: PREVIEW_READ_ONLY_MESSAGE,
  },
};

/** 预览模式下把真实调用替换成假数据；桌面应用原样返回真实结果。 */
export async function adminStatusForDisplay(): Promise<AdminState> {
  if (previewMode === "keychain") {
    throw new BackendError(
      "KEYCHAIN_MISSING",
      "钥匙串里找不到这把加密库的密钥，本机数据暂时读不出来。",
    );
  }
  if (previewMode) return PREVIEW_STATUS[previewMode];
  return api.adminStatus();
}
