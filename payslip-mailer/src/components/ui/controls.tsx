import {
  forwardRef,
  useState,
  type ButtonHTMLAttributes,
  type InputHTMLAttributes,
  type ReactNode,
  type SelectHTMLAttributes,
} from "react";
import s from "./controls.module.css";

/* ============================================================
   控件基元 · 规范来源：设计文档 ch.11 组件规范
   ============================================================ */

/* ---------------- Button ---------------- */

type ButtonVariant = "default" | "primary" | "ghost" | "danger";

export interface ButtonProps extends ButtonHTMLAttributes<HTMLButtonElement> {
  variant?: ButtonVariant;
  size?: "md" | "sm";
  block?: boolean;
  loading?: boolean;
}

const variantClass: Record<ButtonVariant, string | undefined> = {
  default: undefined,
  primary: s.btnPri,
  ghost: s.btnGhost,
  danger: s.btnDgr,
};

export const Button = forwardRef<HTMLButtonElement, ButtonProps>(function Button(
  { variant = "default", size = "md", block, loading, children, className, disabled, ...rest },
  ref,
) {
  const cls = [
    s.btn,
    variantClass[variant],
    size === "sm" ? s.btnSm : undefined,
    block ? s.btnBlock : undefined,
    className,
  ]
    .filter(Boolean)
    .join(" ");

  return (
    <button ref={ref} className={cls} disabled={disabled || loading} {...rest}>
      {loading && <span className={s.btnSpinner} aria-hidden />}
      {children}
    </button>
  );
});

/* ---------------- Field ---------------- */

export interface FieldProps {
  label?: ReactNode;
  required?: boolean;
  error?: string;
  hint?: ReactNode;
  children: ReactNode;
  htmlFor?: string;
}

/** 标签 + 控件 + 行下说明；错误时以红描边 + 文字说明呈现，不只靠颜色 */
export function Field({ label, required, error, hint, children, htmlFor }: FieldProps) {
  return (
    <div className={s.field}>
      {label && (
        <label className={s.label} htmlFor={htmlFor}>
          {label}
          {required && (
            <span className={s.required} aria-hidden>
              *
            </span>
          )}
        </label>
      )}
      {children}
      {error ? (
        <div className={s.errorText} role="alert">
          {error}
        </div>
      ) : (
        hint && <div className={s.hint}>{hint}</div>
      )}
    </div>
  );
}

/* ---------------- Input ---------------- */

export interface InputProps extends InputHTMLAttributes<HTMLInputElement> {
  invalid?: boolean;
  readOnly?: boolean;
}

export const Input = forwardRef<HTMLInputElement, InputProps>(function Input(
  { invalid, readOnly, className, ...rest },
  ref,
) {
  const cls = [
    s.input,
    invalid ? s.inputError : undefined,
    readOnly ? s.inputReadonly : undefined,
    className,
  ]
    .filter(Boolean)
    .join(" ");
  return <input ref={ref} className={cls} aria-invalid={invalid || undefined} {...rest} />;
});

/* ---------------- PasswordInput（带显隐切换）---------------- */

export const PasswordInput = forwardRef<HTMLInputElement, InputProps>(function PasswordInput(
  { invalid, className, ...rest },
  ref,
) {
  const [shown, setShown] = useState(false);
  return (
    <div className={s.inputWrap}>
      <input
        ref={ref}
        type={shown ? "text" : "password"}
        className={[s.input, invalid ? s.inputError : undefined, className]
          .filter(Boolean)
          .join(" ")}
        aria-invalid={invalid || undefined}
        {...rest}
      />
      <button
        type="button"
        className={s.revealBtn}
        onClick={() => setShown((v) => !v)}
        aria-label={shown ? "隐藏密码" : "显示密码"}
      >
        {shown ? "隐藏" : "显示"}
      </button>
    </div>
  );
});

/* ---------------- Select ---------------- */

export interface SelectProps extends SelectHTMLAttributes<HTMLSelectElement> {
  invalid?: boolean;
}

export const Select = forwardRef<HTMLSelectElement, SelectProps>(function Select(
  { invalid, className, children, ...rest },
  ref,
) {
  const cls = [s.select, invalid ? s.inputError : undefined, className]
    .filter(Boolean)
    .join(" ");
  return (
    <select ref={ref} className={cls} aria-invalid={invalid || undefined} {...rest}>
      {children}
    </select>
  );
});

/* ---------------- Switch ---------------- */

export interface SwitchProps {
  checked: boolean;
  onChange: (next: boolean) => void;
  label?: string;
  disabled?: boolean;
}

export function Switch({ checked, onChange, label, disabled }: SwitchProps) {
  return (
    <button
      type="button"
      role="switch"
      aria-checked={checked}
      aria-label={label}
      disabled={disabled}
      className={[s.switch, checked ? s.switchOn : undefined].filter(Boolean).join(" ")}
      onClick={() => onChange(!checked)}
    >
      <i />
    </button>
  );
}

/* ---------------- Row ---------------- */

export function Row({ children, fill, className }: { children: ReactNode; fill?: boolean; className?: string }) {
  return (
    <div className={[s.row, fill ? s.rowFill : undefined, className].filter(Boolean).join(" ")}>
      {children}
    </div>
  );
}
