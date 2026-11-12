import type { ReactNode } from "react";
import s from "./display.module.css";

/* ============================================================
   展示型基元 · 规范来源：设计文档 ch.11 组件规范
   语义色只用于状态；金额不使用红绿表达涨跌
   ============================================================ */

/* ---------------- Card ---------------- */

export type CardTone = "default" | "accent" | "warn" | "danger" | "ok";

export interface CardProps {
  title?: ReactNode;
  meta?: ReactNode;
  actions?: ReactNode;
  tone?: CardTone;
  flush?: boolean;
  children?: ReactNode;
  className?: string;
}

const toneClass: Record<CardTone, string | undefined> = {
  default: undefined,
  accent: s.cardAcc,
  warn: s.cardWarn,
  danger: s.cardDang,
  ok: s.cardOk,
};

export function Card({
  title,
  meta,
  actions,
  tone = "default",
  flush,
  children,
  className,
}: CardProps) {
  const cls = [s.card, toneClass[tone], flush ? s.cardFlush : undefined, className]
    .filter(Boolean)
    .join(" ");

  return (
    <section className={cls}>
      {(title || meta || actions) && (
        <header className={[s.cardHeader, flush ? s.cardFlush : undefined].filter(Boolean).join(" ")}>
          {title && <span>{title}</span>}
          {meta && <span className={s.cardMeta}>{meta}</span>}
          <span className={s.cardHeaderSpacer} />
          {actions}
        </header>
      )}
      {children}
    </section>
  );
}

/* ---------------- Chip（状态标签）---------------- */

export type ChipTone = "neutral" | "ok" | "warn" | "danger" | "info" | "accent";

export function Chip({ tone = "neutral", children }: { tone?: ChipTone; children: ReactNode }) {
  const cls = [
    s.chip,
    tone === "ok" ? s.chipOk : undefined,
    tone === "warn" ? s.chipWarn : undefined,
    tone === "danger" ? s.chipDang : undefined,
    tone === "info" ? s.chipInfo : undefined,
    tone === "accent" ? s.chipAcc : undefined,
  ]
    .filter(Boolean)
    .join(" ");
  return <span className={cls}>{children}</span>;
}

/* ---------------- Table ---------------- */

export interface Column<T> {
  key: string;
  header: ReactNode;
  /** 金额列：等宽右对齐 */
  numeric?: boolean;
  strong?: boolean;
  width?: number | string;
  render: (row: T, index: number) => ReactNode;
}

export interface TableProps<T> {
  columns: Column<T>[];
  rows: T[];
  rowKey: (row: T, index: number) => string;
  selectedKey?: string | null;
  empty?: ReactNode;
  footer?: ReactNode;
}

export function Table<T>({
  columns,
  rows,
  rowKey,
  selectedKey,
  empty,
  footer,
}: TableProps<T>) {
  if (rows.length === 0 && empty) {
    return <>{empty}</>;
  }

  return (
    <div>
      <div className={s.tableWrap}>
        <table className={s.table}>
          <thead>
            <tr>
              {columns.map((c) => (
                <th key={c.key} style={c.width ? { width: c.width } : undefined}>
                  {c.header}
                </th>
              ))}
            </tr>
          </thead>
          <tbody>
            {rows.map((row, i) => {
              const key = rowKey(row, i);
              return (
                <tr key={key} className={selectedKey === key ? s.selected : undefined}>
                  {columns.map((c) => (
                    <td
                      key={c.key}
                      className={[c.numeric ? "num" : undefined, c.strong ? s.strong : undefined]
                        .filter(Boolean)
                        .join(" ")}
                    >
                      {c.render(row, i)}
                    </td>
                  ))}
                </tr>
              );
            })}
          </tbody>
        </table>
      </div>
      {footer && <div className={s.tableFooter}>{footer}</div>}
    </div>
  );
}

/* ---------------- Progress ---------------- */

export function Progress({
  value,
  tone = "accent",
}: {
  value: number;
  tone?: "accent" | "ok" | "warn" | "danger";
}) {
  const pct = Math.max(0, Math.min(100, value));
  const cls = [
    s.progress,
    tone === "ok" ? s.progressOk : undefined,
    tone === "warn" ? s.progressWarn : undefined,
    tone === "danger" ? s.progressDang : undefined,
  ]
    .filter(Boolean)
    .join(" ");
  return (
    <div
      className={cls}
      role="progressbar"
      aria-valuenow={Math.round(pct)}
      aria-valuemin={0}
      aria-valuemax={100}
    >
      <i className={s.progressBar} style={{ width: `${pct}%` }} />
    </div>
  );
}

/* ---------------- Steps（步骤条）---------------- */

export interface StepItem {
  label: string;
  state: "pending" | "active" | "done";
}

export function Steps({ items }: { items: StepItem[] }) {
  return (
    <div className={s.steps}>
      {items.map((item, i) => (
        <div key={item.label} style={{ display: "contents" }}>
          {i > 0 && <span className={s.stepLine} aria-hidden />}
          <div
            className={[
              s.step,
              item.state === "active" ? s.stepActive : undefined,
              item.state === "done" ? s.stepDone : undefined,
            ]
              .filter(Boolean)
              .join(" ")}
          >
            <span className={s.stepCircle}>{item.state === "done" ? "✓" : i + 1}</span>
            <span>{item.label}</span>
          </div>
        </div>
      ))}
    </div>
  );
}

/* ---------------- Stat ---------------- */

export function StatGrid({ children }: { children: ReactNode }) {
  return <div className={s.statGrid}>{children}</div>;
}

export function Stat({
  value,
  label,
  tone,
}: {
  value: ReactNode;
  label: string;
  tone?: "accent" | "ok" | "warn";
}) {
  return (
    <div className={s.stat}>
      <div
        className={[
          s.statValue,
          tone === "accent" ? s.statAcc : undefined,
          tone === "ok" ? s.statOk : undefined,
          tone === "warn" ? s.statWarn : undefined,
        ]
          .filter(Boolean)
          .join(" ")}
      >
        {value}
      </div>
      <div className={s.statLabel}>{label}</div>
    </div>
  );
}

/* ---------------- Empty（空状态）---------------- */

export function Empty({
  icon,
  title,
  hint,
  action,
}: {
  icon?: ReactNode;
  title: ReactNode;
  hint?: ReactNode;
  action?: ReactNode;
}) {
  return (
    <div className={s.empty}>
      {icon && <span className={s.emptyIcon}>{icon}</span>}
      <div>{title}</div>
      {hint && <div className={s.emptyHint}>{hint}</div>}
      {action && <div style={{ marginTop: 12 }}>{action}</div>}
    </div>
  );
}

export function Divider() {
  return <div className={s.divider} />;
}
