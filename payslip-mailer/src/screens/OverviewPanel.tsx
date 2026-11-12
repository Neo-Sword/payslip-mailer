import { useCallback, useEffect, useState } from "react";
import { Button, Card, Chip, Column, Empty, Progress, Stat, StatGrid, Steps, Table } from "../components/ui";
import { PaneHead } from "../components/layout/AppShell";
import { api, BackendError, type HistoryEntry, type PeriodRow, type PeriodTotals, type RecordRow } from "../lib/api";
import s from "./OverviewPanel.module.css";

/* ============================================================
   S03 · 工作台首页（TC-602）

   原来这一屏是**整屏假数据**：四个统计块写死 42/3/10/100%，
   「最近发送批次」是常量数组，发薪流程的步骤条也是写死的。
   一行假的「2026-10 工资条 · 全部成功」会让人以为账已经发过了 ——
   比没有数字更糟。TC-602 把它们全部接到后端。

   这里的数据来自七个已有 Command（**不新增任何 Rust 接口**）：
   member_count / list_groups / list_fields / list_send_history /
   list_periods / list_records / get_period_totals。
   ============================================================ */

/** 批次 → 月份账套的年月（月份名来自 list_periods） */
function batchMonth(batchId: number, periods: PeriodRow[]): string {
  const p = periods.find((x) => x.id === batchId);
  return p ? p.yearMonth : "—";
}

/** epoch 秒 → 本地时间字符串（YYYY-MM-DD HH:MM） */
function fmtTime(secs: number | null): string {
  if (secs === null) return "—";
  const d = new Date(secs * 1000);
  const pad = (n: number) => String(n).padStart(2, "0");
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())} ${pad(d.getHours())}:${pad(d.getMinutes())}`;
}

/** 「今天」的中文写法（概览屏顶部那句） */
function todayText(): string {
  const d = new Date();
  return `${d.getFullYear()} 年 ${d.getMonth() + 1} 月 ${d.getDate()} 日`;
}

interface BatchLine {
  id: number;
  periodId: number;
  status: string;
  total: number;
  success: number;
  failed: number;
  startedAt: number | null;
}

const STATUS_LABEL: Record<string, string> = {
  running: "进行中",
  paused: "已暂停",
  aborted: "已中止",
  finished: "已完成",
};

const STATUS_TONE: Record<string, "ok" | "warn" | "danger" | "info" | "neutral"> = {
  running: "info",
  paused: "warn",
  aborted: "danger",
  finished: "ok",
};

/** 账套状态 → 中文（与「月度工资表」那一屏同一套词） */
const PERIOD_STATUS_LABEL: Record<string, string> = {
  draft: "草稿",
  ready: "待发送",
  sending: "发送中",
  sent: "已发送",
  archived: "已归档",
};

export function OverviewPanel({
  onGoPeriods,
  onGoImport,
  onGoPreview,
}: {
  onGoPeriods?: () => void;
  onGoImport?: () => void;
  onGoPreview?: () => void;
}) {
  const [members, setMembers] = useState<number | null>(null);
  const [groupCount, setGroupCount] = useState<number | null>(null);
  const [fieldCount, setFieldCount] = useState<number | null>(null);
  const [history, setHistory] = useState<HistoryEntry[]>([]);
  const [periods, setPeriods] = useState<PeriodRow[]>([]);
  const [records, setRecords] = useState<RecordRow[]>([]);
  const [totals, setTotals] = useState<PeriodTotals | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [loaded, setLoaded] = useState(false);

  const refresh = useCallback(async () => {
    try {
      const [m, g, f, h, ps] = await Promise.all([
        api.memberCount(),
        api.listGroups(),
        api.listFields(),
        api.listSendHistory(),
        api.listPeriods(),
      ]);
      setMembers(m);
      setGroupCount(g.length);
      // 「已配置工资项」数的是**启用中**的字段 —— 停用的字段不进表头（ADR-0015）。
      setFieldCount(f.filter((x) => x.visible).length);
      setHistory(h);
      setPeriods(ps);
      setError(null);
      setLoaded(true);
    } catch (raw) {
      setError((raw as BackendError).message || "读工作台数据失败。");
    }
  }, []);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  /** 当前在办的账套：优先 draft/ready（能改的），否则取列表第一个。 */
  const current = periods.find((p) => p.status === "draft" || p.status === "ready") ?? periods[0] ?? null;

  useEffect(() => {
    if (current === null) {
      setRecords([]);
      setTotals(null);
      return;
    }
    void api
      .listRecords(current.id)
      .then(setRecords)
      .catch(() => setRecords([]));
    void api
      .getPeriodTotals(current.id)
      .then(setTotals)
      .catch(() => setTotals(null));
  }, [current]);

  /** 最近一个批次 → 送达率（没有批次时是 null，界面显示「—」而不是 100%）。 */
  const latest = history[0] ?? null;
  const rate =
    latest && latest.progress.total > 0
      ? Math.round((latest.progress.success / latest.progress.total) * 100)
      : null;

  /** 发薪流程四步的状态：全部由可观察的事实推导，不写死。 */
  const hasBatchForPeriod = current ? history.some((h) => h.batch.periodId === current.id) : false;
  const sent = current?.status === "sent" || current?.status === "archived";
  const imported = records.length > 0;
  const steps = [
    { label: "导出模板", state: (current ? "done" : "pending") as "pending" | "active" | "done" },
    { label: "导入数据", state: (imported ? "done" : current ? "active" : "pending") as "pending" | "active" | "done" },
    { label: "预览核对", state: (hasBatchForPeriod ? "done" : imported ? "active" : "pending") as "pending" | "active" | "done" },
    { label: "发送完成", state: (sent ? "done" : "pending") as "pending" | "active" | "done" },
  ];

  const batchLines: BatchLine[] = history.slice(0, 8).map((h) => ({
    id: h.batch.id,
    periodId: h.batch.periodId,
    status: h.batch.status,
    total: h.batch.total,
    success: h.batch.success,
    failed: h.batch.failed,
    startedAt: h.batch.startedAt,
  }));

  const columns: Column<BatchLine>[] = [
    { key: "id", header: "批次", render: (b) => <b>#{b.id}</b> },
    { key: "month", header: "月份", render: (b) => batchMonth(b.periodId, periods) },
    { key: "status", header: "状态", render: (b) => <Chip tone={STATUS_TONE[b.status] ?? "neutral"}>{STATUS_LABEL[b.status] ?? b.status}</Chip> },
    { key: "total", header: "总数", numeric: true, render: (b) => b.total },
    { key: "success", header: "成功", numeric: true, render: (b) => b.success },
    { key: "failed", header: "失败", numeric: true, render: (b) => (b.failed > 0 ? <span className={s.failed}>{b.failed}</span> : 0) },
    { key: "startedAt", header: "开始时间", render: (b) => fmtTime(b.startedAt) },
  ];

  /** 本月进度条：已导入行数相对于「在册成员数」。没有成员时不做比例。 */
  const pct = members && members > 0 ? Math.min(100, Math.round((records.length / members) * 100)) : 0;

  return (
    <>
      <PaneHead
        title="早上好，管理员"
        sub={todayText()}
        actions={
          <Button variant="primary" onClick={() => onGoPeriods?.()}>
            ＋ 新建月份账套
          </Button>
        }
      />

      <StatGrid>
        <Stat value={members ?? "—"} label="在册成员" />
        <Stat value={groupCount ?? "—"} label="邮件组" />
        <Stat value={fieldCount ?? "—"} label="已配置工资项" tone="accent" />
        <Stat value={rate === null ? "—" : `${rate}%`} label="最近批次送达率" tone={rate === null ? undefined : rate === 100 ? "ok" : "warn"} />
      </StatGrid>

      <Card
        title={current ? `${current.yearMonth} · 发薪流程` : "还没有月份账套"}
        actions={
          current ? (
            <Chip tone={current.status === "sent" || current.status === "archived" ? "ok" : "neutral"}>
              {PERIOD_STATUS_LABEL[current.status] ?? current.status}
            </Chip>
          ) : undefined
        }
      >
        <Steps items={steps} />
        <div className={s.hint}>
          {current === null
            ? "先建一个月份账套（比如 2026-12），然后导空模板给财务填。"
            : imported
              ? `已导入 ${records.length} 行` +
                (totals
                  ? ` · 应发合计 ${totals.earnings.toLocaleString("zh-CN")} · 扣款合计 ${totals.deductions.toLocaleString("zh-CN")} · 实发合计 ${totals.net.toLocaleString("zh-CN")}`
                  : "")
              : "这个月还没有导入任何数据。导空模板给财务，填完再导回来。"}
        </div>
      </Card>

      <Card
        title="最近发送批次"
        flush
        actions={
          <span className={s.hint}>
            {batchLines.length > 0 ? "查看全部 →" : "还没有批次"}
          </span>
        }
      >
        {batchLines.length === 0 ? (
          <div className={s.emptyBox}>
            <Empty
              icon="≡"
              title="还没有任何发送批次"
              hint="在「预览与核对」里核对通过后点「发送」，批次会出现在这里"
            />
          </div>
        ) : (
          <Table columns={columns} rows={batchLines} rowKey={(b) => String(b.id)} />
        )}
      </Card>

      <Card title="本月发薪进度" tone="accent">
        {current === null ? (
          <div className={s.hint}>还没有本月的账套。</div>
        ) : (
          <>
            <div className={s.hint}>
              · {current.yearMonth}：已导入 {records.length} 行 / 在册 {members ?? "—"} 人
              {hasBatchForPeriod ? " · 已开始发送" : " · 还没发送"}
            </div>
            <div className={s.progressWrap}>
              <Progress value={pct} />
            </div>
            <div className={s.actions}>
              <Button size="sm" onClick={() => onGoImport?.()}>
                去导入数据
              </Button>
              <Button size="sm" onClick={() => onGoPreview?.()}>
                去预览核对
              </Button>
            </div>
          </>
        )}
      </Card>

      {error && (
        <div className={s.error} role="alert">
          {error}
        </div>
      )}
      {!loaded && !error && <div className={s.hint}>正在读本机数据…</div>}
    </>
  );
}
