import { useCallback, useEffect, useState } from "react";
import { Button, Card, Chip, Column, Empty, Table } from "../components/ui";
import { api, BackendError, type BatchRow } from "../lib/api";
import s from "./HistoryPanel.module.css";

/* ============================================================
   发送队列（TC-400 / TC-402 的界面侧）

   侧边栏有这一项，但原来**没有对应界面**（点进去是占位页）。
   这一屏做的是「正在发 / 暂停了 / 中止了」的批次视图：
   暂停、继续、中止、**仅重试失败项**（红线 4：不提供整批重发）。

   与「发送历史」的分界：历史是**已经结束**的批次（可看逐封明细、可导凭证），
   队列是**还能动手**的批次。两者都只显示状态与计数 ——
   这一屏**没有工资数值**（ADR-0026 的脱敏口径）。
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

/** epoch 秒 → 本地时间字符串（YYYY-MM-DD HH:MM） */
function fmtTime(secs: number | null): string {
  if (secs === null) return "—";
  const d = new Date(secs * 1000);
  const pad = (n: number) => String(n).padStart(2, "0");
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())} ${pad(d.getHours())}:${pad(d.getMinutes())}`;
}

/** 这一行的批次名（月份账套 id → 年月） */
function batchName(b: BatchRow, names: Map<number, string>): string {
  return names.get(b.periodId) ?? `账套 #${b.periodId}`;
}

export function QueuePanel() {
  const [batches, setBatches] = useState<BatchRow[]>([]);
  const [names, setNames] = useState<Map<number, string>>(new Map());
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);

  const refresh = useCallback(async () => {
    try {
      const [list, periods] = await Promise.all([api.listSendBatches(), api.listPeriods()]);
      setBatches(list);
      setNames(new Map(periods.map((p) => [p.id, p.yearMonth])));
      setError(null);
    } catch (raw) {
      setError((raw as BackendError).message || "读发送队列失败。");
    }
  }, []);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  /* 有批次在跑时自动刷新（2 秒一次）；停下来就不轮询了 —— 空闲时不打扰库。 */
  const running = batches.some((b) => b.status === "running");
  useEffect(() => {
    if (!running) return;
    const t = setInterval(() => void refresh(), 2000);
    return () => clearInterval(t);
  }, [running, refresh]);

  const act = async (batchId: number, kind: "pause" | "resume" | "abort" | "retry") => {
    setBusy(true);
    setError(null);
    setNotice(null);
    try {
      if (kind === "pause") {
        await api.pauseSendBatch(batchId);
        setNotice("已暂停：正在发的那封会发完，后面的不再发。");
      } else if (kind === "resume") {
        await api.resumeSendBatch(batchId);
        setNotice("已继续。");
      } else if (kind === "abort") {
        await api.abortSendBatch(batchId);
        setNotice("已中止。剩下的不会发出（凭据坏的时候，重试没有意义）。");
      } else {
        const n = await api.retryFailedJobs(batchId);
        setNotice(n > 0 ? `已把 ${n} 封失败的重新排进队列。` : "这个批次没有可重试的失败项。");
      }
      await refresh();
    } catch (raw) {
      setError((raw as BackendError).message || "操作没成功，请重试。");
    } finally {
      setBusy(false);
    }
  };

  const columns: Column<BatchRow>[] = [
    { key: "id", header: "批次", render: (b) => <b>#{b.id}</b> },
    { key: "period", header: "月份", render: (b) => batchName(b, names) },
    { key: "status", header: "状态", render: (b) => <Chip tone={STATUS_TONE[b.status] ?? "neutral"}>{STATUS_LABEL[b.status] ?? b.status}</Chip> },
    { key: "total", header: "总数", numeric: true, render: (b) => b.total },
    { key: "success", header: "已发", numeric: true, render: (b) => b.success },
    { key: "failed", header: "失败", numeric: true, render: (b) => (b.failed > 0 ? <span className={s.failed}>{b.failed}</span> : 0) },
    { key: "startedAt", header: "开始时间", render: (b) => fmtTime(b.startedAt) },
    {
      key: "ops",
      header: "操作",
      render: (b) => (
        <span style={{ display: "inline-flex", gap: 6 }}>
          {b.status === "running" && (
            <>
              <Button size="sm" disabled={busy} onClick={() => void act(b.id, "pause")}>
                暂停
              </Button>
              <Button size="sm" variant="danger" disabled={busy} onClick={() => void act(b.id, "abort")}>
                中止
              </Button>
            </>
          )}
          {b.status === "paused" && (
            <>
              <Button size="sm" disabled={busy} onClick={() => void act(b.id, "resume")}>
                继续
              </Button>
              <Button size="sm" variant="danger" disabled={busy} onClick={() => void act(b.id, "abort")}>
                中止
              </Button>
            </>
          )}
          {b.failed > 0 && b.status !== "running" && (
            <Button size="sm" disabled={busy} onClick={() => void act(b.id, "retry")}>
              仅重试失败项
            </Button>
          )}
        </span>
      ),
    },
  ];

  return (
    <div className={s.wrap}>
      <Card
        title="发送队列"
        actions={
          <Button size="sm" disabled={busy} onClick={() => void refresh()}>
            刷新
          </Button>
        }
      >
        {batches.length === 0 ? (
          <Empty
            icon="≡"
            title="队列里没有批次"
            hint="在「预览与核对」里核对通过后点「发送」，批次会出现在这里"
          />
        ) : (
          <Table columns={columns} rows={batches} rowKey={(b) => String(b.id)} />
        )}

        <p className={s.note}>
          「仅重试失败项」只把<b>没发成功</b>的那些重新排队（红线 4：不提供整批重发）。
          已成功的<b>绝不</b>重发。
        </p>

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
      </Card>
    </div>
  );
}
