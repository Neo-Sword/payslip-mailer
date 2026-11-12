import { useCallback, useEffect, useState } from "react";
import { Button, Card, Chip, ConfirmDialog, Empty, Select } from "../components/ui";
import {
  api,
  BackendError,
  type BatchProgress,
  type BatchRow,
  type MemberVerification,
  type PeriodRow,
  type ScopeVerification,
} from "../lib/api";
import s from "./VerifyPanel.module.css";

/* ============================================================
   S11 · 预览与核对（TC-401 · ADR-0024）
   发送前的最后一道闸：三重核对在这里兑现。

   - 身份一致：身份待核对 / 已停用 / 校验列有问题的行 —— 拦在范围外。
   - 变量完整：required 变量空 / 模板渲染失败 —— 拦。
   - 内容一致：checksum 在发送编排（TC-402）逐封比对。

   核对看到的与发出去的是**同一份渲染**（同一个 context_for）。
   ============================================================ */

export function VerifyPanel() {
  const [periods, setPeriods] = useState<PeriodRow[]>([]);
  const [periodId, setPeriodId] = useState<number | null>(null);
  const [report, setReport] = useState<ScopeVerification | null>(null);
  const [selected, setSelected] = useState<MemberVerification | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  // 发送编排（TC-402）
  const [unfinished, setUnfinished] = useState<BatchProgress & { batch: BatchRow } | null>(null);
  const [confirmOpen, setConfirmOpen] = useState(false);
  const [batch, setBatch] = useState<BatchRow | null>(null);

  const refreshUnfinished = useCallback(async () => {
    try {
      const u = await api.findUnfinishedBatch();
      setUnfinished(u ? { ...u.progress, batch: u.batch } : null);
    } catch {
      setUnfinished(null);
    }
  }, []);

  const refreshPeriods = useCallback(async () => {
    try {
      const list = await api.listPeriods();
      setPeriods(list);
      setPeriodId((prev) => {
        if (prev !== null && list.some((p) => p.id === prev)) return prev;
        return list[0]?.id ?? null;
      });
    } catch (raw) {
      setError((raw as BackendError).message || "读月份账套失败。");
    }
    await refreshUnfinished();
  }, [refreshUnfinished]);

  useEffect(() => {
    void refreshPeriods();
  }, [refreshPeriods]);

  /** 核对整个发送范围（v1 范围 = 全部启用成员；组/多选的范围选择归 TC-402）。 */
  const runVerify = useCallback(async () => {
    if (periodId === null) return;
    setBusy(true);
    setError(null);
    setNotice(null);
    setSelected(null);
    try {
      const members = await api.listSendableMembers();
      const ids = members.map((m) => m.id);
      if (ids.length === 0) {
        setError("名单里没有启用成员 —— 先在「邮件组与成员」里添加人。");
        return;
      }
      const r = await api.verifySendScope(periodId, ids, null);
      setReport(r);
      setNotice(
        r.ok
          ? `核对通过：全部 ${r.total} 人都能发送。`
          : `核对完成：${r.sendableCount}/${r.total} 人可发送，${r.blocked.length} 人被拦。`,
      );
    } catch (raw) {
      setError((raw as BackendError).message || "核对失败。");
    } finally {
      setBusy(false);
    }
  }, [periodId]);

  useEffect(() => {
    setReport(null);
    setSelected(null);
  }, [periodId]);

  /* ---------------- 发送编排（TC-402）---------------- */

  /** 确认后的真正入口：start_batch（Command 层会再核对一遍 —— 那才是关卡）。 */
  const doStart = useCallback(async () => {
    if (periodId === null || report === null) return;
    setBusy(true);
    setError(null);
    try {
      const ids = report.members.map((m) => m.memberId);
      const b = await api.startBatch(periodId, ids, null);
      setBatch(b);
      setConfirmOpen(false);
      setNotice(
        `批次已创建：${b.total} 个任务已排队。真正的发送要等发信邮箱配置好（设置页）。`,
      );
      await refreshUnfinished();
    } catch (raw) {
      setError((raw as BackendError).message || "创建批次失败。");
      setConfirmOpen(false);
    } finally {
      setBusy(false);
    }
  }, [periodId, report, refreshUnfinished]);

  /** 恢复未完成的批次。TC-500 之前必然收到「发信设置还没配置」—— 那是事实。 */
  const doResume = useCallback(async () => {
    if (unfinished === null) return;
    setBusy(true);
    setError(null);
    try {
      await api.recoverAndRunBatch(unfinished.batch.id);
      await refreshUnfinished();
    } catch (raw) {
      const err = raw as BackendError;
      // 「SMTP 未配置」不是故障 —— 是 TC-500 之前的常态，说清楚就好
      if (err.code === "VALIDATION_ERROR" && err.message.includes("发信设置")) {
        setNotice(err.message);
        await refreshUnfinished();
      } else {
        setError(err.message || "恢复失败。");
      }
    } finally {
      setBusy(false);
    }
  }, [unfinished, refreshUnfinished]);

  /** 仅重试失败项（红线 4：只补未成功的）。 */
  const doRetryFailed = useCallback(async () => {
    if (unfinished === null) return;
    setBusy(true);
    setError(null);
    try {
      const n = await api.retryFailedJobs(unfinished.batch.id);
      setNotice(`已把 ${n} 个失败任务重新排队。`);
      await refreshUnfinished();
    } catch (raw) {
      setError((raw as BackendError).message || "重试失败。");
    } finally {
      setBusy(false);
    }
  }, [unfinished, refreshUnfinished]);

  return (
    <div className={s.wrap}>
      <Card
        title="预览与核对"
        meta="发送前的三重核对：身份 / 内容 / 变量"
        actions={
          <Button variant="primary" loading={busy} disabled={periodId === null} onClick={runVerify}>
            ✓ 核对发送范围
          </Button>
        }
      >
        <div className={s.periodRow}>
          <span className={s.periodLabel}>核对</span>
          <Select value={periodId ?? ""} onChange={(e) => setPeriodId(Number(e.target.value))}>
            {periods.length === 0 && <option value="">（还没有月份账套）</option>}
            {periods.map((p) => (
              <option key={p.id} value={p.id}>
                {p.yearMonth}
              </option>
            ))}
          </Select>
          <span className={s.hint}>
            v1 核对全部启用成员；按组 / 多选成员的范围选择在发送编排里。
          </span>
        </div>

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

      {/* 未完成批次（ch.7：「发现未完成的批次：已发 27/60，是否继续？」） */}
      {unfinished && (
        <Card
          title="发现未完成的批次"
          meta={`批次 #${unfinished.batch.id} · ${unfinished.batch.status === "paused" ? "已暂停" : "进行中"}`}
          tone="warn"
          actions={
            <>
              <Button size="sm" loading={busy} onClick={() => void doResume()}>
                继续
              </Button>
              <Button size="sm" loading={busy} onClick={() => void doRetryFailed()}>
                仅重试失败项
              </Button>
            </>
          }
        >
          <div className={s.progressLine}>
            已发 <b>{unfinished.success}</b> / {unfinished.total} 封
            {unfinished.failed > 0 && <> · 失败 <b className={s.warnText}>{unfinished.failed}</b></>}
            {unfinished.pending > 0 && <> · 待发 {unfinished.pending}</>}
            {unfinished.inFlight > 0 && <> · 进行中 {unfinished.inFlight}</>}
          </div>
          <p className={s.hint}>
            已成功的人不会收到第二封；继续前存疑的任务会回到队列。
          </p>
        </Card>
      )}

      {report && (
        <Card
          title="发送"
          meta={
            report.ok
              ? `核对通过 · ${report.sendableCount} 人`
              : `核对未通过 · ${report.blocked.length} 人被拦`
          }
          actions={
            <Button
              variant="primary"
              disabled={!report.ok || busy}
              loading={busy}
              onClick={() => setConfirmOpen(true)}
            >
              ➤ 发送
            </Button>
          }
        >
          {!report.ok ? (
            <p className={s.hint}>
              有 {report.blocked.length} 人被拦（身份 / 校验 / 变量），处理后再发送。
            </p>
          ) : batch ? (
            <div className={s.progressLine}>
              批次 #{batch.id}：{batch.total} 个任务已排队
              {batch.status === "running" && <>（等待发信邮箱配置后开始发送）</>}
            </div>
          ) : (
            <p className={s.hint}>
              点「发送」前会再让你确认一次：本次将向 {report.sendableCount} 人发送{" "}
              {report.sendableCount} 封。
            </p>
          )}
        </Card>
      )}

      {report && (
        <div className={s.split}>
          <Card
            title="核对结果"
            meta={`${report.sendableCount}/${report.total} 可发送`}
            flush
            className={s.listCard}
          >
            {report.members.length === 0 ? (
              <Empty icon="➤" title="还没有结果" hint="点「核对发送范围」开始。" />
            ) : (
              <ul className={s.list}>
                {report.members.map((m) => (
                  <li key={m.memberId}>
                    <button
                      type="button"
                      className={[s.item, selected?.memberId === m.memberId ? s.itemActive : undefined]
                        .filter(Boolean)
                        .join(" ")}
                      onClick={() => setSelected(m)}
                    >
                      <span className={s.itemName}>
                        {m.name}
                        <span className={s.itemEmail}>{m.email}</span>
                      </span>
                      {m.sendable ? (
                        <Chip tone="ok">通过</Chip>
                      ) : (
                        <Chip tone="warn">被拦</Chip>
                      )}
                    </button>
                  </li>
                ))}
              </ul>
            )}
          </Card>

          <Card
            title={selected ? `${selected.name} 的工资条预览` : "预览"}
            meta={selected?.sendable ? undefined : selected ? "这个人被拦了" : undefined}
            className={s.previewCard}
          >
            {selected === null ? (
              <div className={s.placeholder}>在左边点一个名字，看这封邮件长什么样。</div>
            ) : selected.preview ? (
              <>
                {selected.blockedReasons.length > 0 && (
                  <div className={s.warn}>
                    {selected.blockedReasons.map((r) => (
                      <div key={r}>{r}</div>
                    ))}
                  </div>
                )}
                {selected.variableProblems.length > 0 && (
                  <div className={s.warn}>
                    {selected.variableProblems.map((r) => (
                      <div key={r}>{r}</div>
                    ))}
                  </div>
                )}
                <div className={s.subject}>
                  <span className={s.subjectLabel}>主题</span>
                  {selected.preview.subject}
                </div>
                <div className={s.htmlBox} aria-label="邮件正文预览">
                  <iframe
                    title="工资条预览"
                    className={s.htmlFrame}
                    sandbox=""
                    srcDoc={selected.preview.html}
                  />
                </div>
              </>
            ) : (
              <div className={s.placeholder}>
                这个人没有可预览的渲染结果：
                <div>{selected.blockedReasons.join("；")}</div>
                <div>{selected.variableProblems.join("；")}</div>
              </div>
            )}
          </Card>
        </div>
      )}
      <ConfirmDialog
        open={confirmOpen}
        title="确认发送工资条"
        danger
        confirmText="确认发送"
        description={`本次将向 ${report?.sendableCount ?? 0} 人发送 ${report?.sendableCount ?? 0} 封。每封邮件只有一个收件人，绝不抄送、绝不密送。发送过程中可以暂停。`}
        onConfirm={() => void doStart()}
        onCancel={() => setConfirmOpen(false)}
      />
    </div>
  );
}
