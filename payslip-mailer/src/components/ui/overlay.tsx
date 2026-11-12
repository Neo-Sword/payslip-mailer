import {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useMemo,
  useRef,
  useState,
  type ReactNode,
} from "react";
import { createPortal } from "react-dom";
import { Button } from "./controls";
import s from "./overlay.module.css";

/* ============================================================
   浮层 · 规范来源：设计文档 ch.11 组件规范 / ch.12 S06 / S15
   ============================================================ */

/* ---------------- Modal ---------------- */

export interface ModalProps {
  open: boolean;
  title: ReactNode;
  onClose: () => void;
  children?: ReactNode;
  footer?: ReactNode;
  wide?: boolean;
}

export function Modal({ open, title, onClose, children, footer, wide }: ModalProps) {
  useEffect(() => {
    if (!open) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [open, onClose]);

  if (!open) return null;

  return createPortal(
    <div className={s.overlay} onMouseDown={(e) => e.target === e.currentTarget && onClose()}>
      <div
        className={[s.modal, wide ? s.modalWide : undefined].filter(Boolean).join(" ")}
        role="dialog"
        aria-modal="true"
      >
        <header className={s.modalHead}>
          {title}
          <button type="button" className={s.modalClose} onClick={onClose} aria-label="关闭">
            ×
          </button>
        </header>
        <div className={s.modalBody}>{children}</div>
        {footer && <footer className={s.modalFoot}>{footer}</footer>}
      </div>
    </div>,
    document.body,
  );
}

/* ---------------- ConfirmDialog ---------------- */

export interface ConfirmDialogProps {
  open: boolean;
  title: string;
  /** 必须说清三要素：发生了什么 / 影响范围 / 下一步 */
  description: ReactNode;
  confirmText?: string;
  danger?: boolean;
  /** 危险操作要求输入管理员密码以继续 */
  requirePassword?: boolean;
  onConfirm: () => void;
  onCancel: () => void;
}

export function ConfirmDialog({
  open,
  title,
  description,
  confirmText = "确认",
  danger,
  requirePassword,
  onConfirm,
  onCancel,
}: ConfirmDialogProps) {
  const [typed, setTyped] = useState("");
  useEffect(() => {
    if (open) setTyped("");
  }, [open]);

  const blocked = requirePassword ? typed.length === 0 : false;

  return (
    <Modal
      open={open}
      title={title}
      onClose={onCancel}
      footer={
        <>
          <Button variant="ghost" onClick={onCancel}>
            取消
          </Button>
          <Button variant={danger ? "danger" : "primary"} onClick={onConfirm} disabled={blocked}>
            {confirmText}
          </Button>
        </>
      }
    >
      <p className={s.confirmText}>{description}</p>
      {requirePassword && (
        <input
          type="password"
          className="s.confirmText"
          style={{
            width: "100%",
            padding: "7px 10px",
            borderRadius: "var(--r-control)",
            border: "1px solid var(--line-input)",
            background: "var(--bg-input)",
            color: "var(--tx-strong)",
            fontFamily: "inherit",
            fontSize: "var(--fs-body)",
            outline: "none",
          }}
          placeholder="管理员密码"
          value={typed}
          onChange={(e) => setTyped(e.target.value)}
          autoFocus
        />
      )}
    </Modal>
  );
}

/* ---------------- Toast ---------------- */

export type ToastTone = "ok" | "warn" | "danger" | "info";

export interface ToastItem {
  id: number;
  tone: ToastTone;
  text: ReactNode;
  action?: { label: string; onClick: () => void };
  /** 成功 3 秒自动消失；失败常驻直到处理 */
  sticky?: boolean;
}

interface ToastApi {
  push: (t: Omit<ToastItem, "id">) => number;
  dismiss: (id: number) => void;
}

const ToastCtx = createContext<ToastApi | null>(null);

export function ToastProvider({ children }: { children: ReactNode }) {
  const [items, setItems] = useState<ToastItem[]>([]);
  const seq = useRef(0);
  const timers = useRef(new Map<number, ReturnType<typeof setTimeout>>());

  const dismiss = useCallback((id: number) => {
    setItems((xs) => xs.filter((x) => x.id !== id));
    const t = timers.current.get(id);
    if (t) {
      clearTimeout(t);
      timers.current.delete(id);
    }
  }, []);

  const push = useCallback(
    (t: Omit<ToastItem, "id">) => {
      const id = ++seq.current;
      setItems((xs) => [...xs, { ...t, id }]);
      if (!t.sticky) {
        timers.current.set(
          id,
          setTimeout(() => dismiss(id), 3000),
        );
      }
      return id;
    },
    [dismiss],
  );

  useEffect(() => {
    const map = timers.current;
    return () => {
      map.forEach(clearTimeout);
      map.clear();
    };
  }, []);

  const api = useMemo(() => ({ push, dismiss }), [push, dismiss]);

  return (
    <ToastCtx.Provider value={api}>
      {children}
      {createPortal(
        <div className={s.toastHost}>
          {items.map((it) => (
            <div
              key={it.id}
              className={[
                s.toast,
                it.tone === "ok" ? s.toastOk : undefined,
                it.tone === "warn" ? s.toastWarn : undefined,
                it.tone === "danger" ? s.toastDang : undefined,
                it.tone === "info" ? s.toastInfo : undefined,
              ]
                .filter(Boolean)
                .join(" ")}
              role="status"
            >
              <span className={s.toastText}>{it.text}</span>
              {it.action && (
                <button type="button" className={s.toastAction} onClick={it.action.onClick}>
                  {it.action.label}
                </button>
              )}
            </div>
          ))}
        </div>,
        document.body,
      )}
    </ToastCtx.Provider>
  );
}

export function useToast(): ToastApi {
  const ctx = useContext(ToastCtx);
  if (!ctx) throw new Error("useToast 必须在 ToastProvider 内使用");
  return ctx;
}
