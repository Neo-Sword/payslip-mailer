import { useCallback, useEffect, useState } from "react";
import { Button, Card, Chip, Column, Empty, Table } from "../components/ui";
import { api, BackendError, type HistoryEntry, type JobDetail} from "../lib/api";
import s from "./HistoryPanel.module.css";

/* ============================================================
   S13 · 发送历史（TC-403 · ADR-0026）
   批次列表（带进度）+ 点开逐封明细 + 导出 CSV 凭证。

   脱敏口径（ADR-0026）：这一屏**没有工资数值** ——
   明细只有「谁、发到哪、回码、时间」；凭证导出同理。
   ============================================================ */

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

const JOB_LABEL: Record<string, string> = {
  queued: "排队中",
  sending: "发送中",
  success: "成功",
  retrying: "等待重试",
  dead: "失败",
};

const JOB_TONE: Record<string, "ok" | "warn" | "danger" | "neutral"> = {
  queued: "neutral",
  sending: "warn",
  success: "ok",
  retrying: "warn",
  dead: "danger",
};

/** 月份账套的年月文本（批次 → 月份列） */
function periodName(periodId: number): string {
  return PERIODS_REF.get(periodId) ?? `账套 #${periodId}`;
}

/** 由组件填充的账套名对照（模块级可变 —— 单一屏内的简单映射） */
let PERIODS_REF = new Map<number, string>();

/** epoch 秒 → 本地时间字符串（YYYY-MM-DD HH:MM） */
function fmtTime(secs: number | null): string {
  if (secs === null) return "—";
  const d = new Date(secs * 1000);
  const pad = (n: number) => String(n).padStart(2, "0");
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())} ${pad(d.getHours())}:${pad(d.getMinutes())}`;
}

const DETAIL_COLUMNS: Column<JobDetail>[] = [
  { key: "name", header: "姓名", render: (r) => <b>{r.name || "—"}</b> },
  { key: "toEmail", header: "收件地址", render: (r) => r.toEmail },
  { key: "status", header: "状态", render: (r) => <Chip tone={JOB_TONE[r.status] ?? "neutral"}>{JOB_LABEL[r.status] ?? r.status}</Chip> },
  { key: "attempts", header: "尝试", numeric: true, render: (r) => (r.attempts > 1 ? `${r.attempts} 次` : "1 次") },
  { key: "smtpCode", header: "SMTP 回码", numeric: true, render: (r) => (r.smtpCode === null ? "—" : r.smtpCode) },
  { key: "errorCode", header: "失败原因", render: (r) => (r.errorCode ? ERROR_LABEL[r.errorCode] ?? r.errorCode : "—") },
  { key: "sentAt", header: "发送时间", render: (r) => fmtTime(r.sentAt) },
];

const ERROR_LABEL: Record<string, string> = {
  transient: "服务器繁忙（已退避重试）",
  permanent: "地址被拒（不重试）",
  auth: "认证失败（检查专用密码）",
  network: "网络中断（已暂停）",
  render: "渲染失败",
  none: "—",
};

export function HistoryPanel() {
  const [history, setHistory] = useState<HistoryEntry[]>([]);
  const [detail, setDetail] = useState<JobDetail[] | null>(null);
  const [detailBatch, setDetailBatch] = useState<HistoryEntry | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);

  const refresh = useCallback(async () => {
    setBusy(true);
    try {
      const [h, p] = await Promise.all([api.listSendHistory(), api.listPeriods()]);
      setHistory(h);
      PERIODS_REF = new Map(p.map((x) => [x.id, x.yearMonth]));
      setError(null);
    } catch (raw) {
      setError((raw as BackendError).message || "读发送历史失败。");
    } finally {
      setBusy(false);
    }
  }, []);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  const openDetail = useCallback(async (entry: HistoryEntry) => {
    setDetailBatch(entry);
    setBusy(true);
    try {
      setDetail(await api.getBatchJobs(entry.batch.id));
    } catch (raw) {
      setError((raw as BackendError).message || "读逐封明细失败。");
    } finally {
      setBusy(false);
    }
  }, []);

  const exportVoucher = useCallback(async () => {
    if (detailBatch === null) return;
    setBusy(true);
    setError(null);
    try {
      const path = await api.exportBatchVoucher(detailBatch.batch.id);
      setNotice(`凭证已导出：${path}`);
    } catch (raw) {
      setError((raw as BackendError).message || "导出凭证失败。");
    } finally {
      setBusy(false);
    }
  }, [detailBatch]);

  const columns: Column<HistoryEntry>[] = [
    {
      key: "id",
      header: "批次",
      render: (r) => <b>#{r.batch.id}</b>,
    },
    {
      key: "period",
      header: "月份",
      render: (r) => periodName(r.batch.periodId),
    },
    {
      key: "status",
      header: "状态",
      render: (r: HistoryEntry) => (
        <Chip tone={STATUS_TONE[r.batch.status] ?? "neutral"}>
          {STATUS_LABEL[r.batch.status] ?? r.batch.status}
        </Chip>
      ),
    },
    {
      key: "progress",
      header: "进度",
      render: (r) => (
        <>
          已发 <b>{r.progress.success}</b> / {r.progress.total}
          {r.progress.failed > 0 && (
            <span className={s.failed}> · 失败 {r.progress.failed}</span>
          )}
        </>
      ),
    },
    {
      key: "rate",
      header: "速率",
      render: (r) => `${r.batch.ratePerMin} 封/分`,
    },
    {
      key: "started",
      header: "开始",
      render: (r) => fmtTime(r.batch.startedAt),
    },
    {
      key: "finished",
      header: "结束",
      render: (r) => fmtTime(r.batch.finishedAt),
    },
    {
      key: "open",
      header: "",
      render: (r) => (
        <Button size="sm" onClick={() => void openDetail(r)}>
          逐封明细
        </Button>
      ),
    },
  ];

  return (
    <div className={s.wrap}>
      <Card
        title="发送历史"
        meta="批次记录 · 留存 ≥2 年（没有删除入口）"
        actions={
          <Button size="sm" loading={busy} onClick={() => void refresh()}>
            刷新
          </Button>
        }
        flush
      >
        {history.length === 0 ? (
          <Empty
            icon="≡"
            title="还没有发送记录"
            hint="发送完成后，这里会留下每封邮件的收件地址、时间与服务器回执 —— 那是工资条送达的证据链。"
          />
        ) : (
          <Table columns={columns} rows={history} rowKey={(r) => String(r.batch.id)} />
        )}
      </Card>

      {detailBatch && (
        <Card
          title={`批次 #${detailBatch.batch.id} · 逐封明细`}
          meta={`${detailBatch.progress.success}/${detailBatch.progress.total} 成功`}
          actions={
            <>
              <Button size="sm" loading={busy} onClick={() => void exportVoucher()}>
                ⇩ 导出 CSV 凭证
              </Button>
              <Button size="sm" variant="ghost" onClick={() => { setDetail(null); setDetailBatch(null); }}>
                关闭
              </Button>
            </>
          }
          flush
        >
          <p className={s.note}>
            凭证里有姓名、收件地址、状态、SMTP 回码、发送时间与内容校验和 ——
            **没有工资数值**（凭证会被存档转发，工资数值只在加密库里）。
          </p>
          {detail && (
            <Table columns={DETAIL_COLUMNS} rows={detail} rowKey={(r) => String(r.jobId)} />
          )}
        </Card>
      )}

      {notice && (
        <div className={s.notice} role="status">
          {notice}
        </div>
      )}
      {error && (
        <div className={s.error} role="alert">
          {error}
        </div>
      )}
    </div>
  );
}
