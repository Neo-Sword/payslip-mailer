import type { ReactNode } from "react";
import { TitleBar } from "./TitleBar";
import s from "./Stage.module.css";

/* ============================================================
   居中舞台 · S01 初始化向导 / S02 锁屏 / KEYCHAIN 引导共用
   没有侧栏（S01 的验收标准就写着「侧栏不出现」），
   标题栏与工作台同一个，所以三屏之间切换不会出现窗口外观跳变。
   ============================================================ */

export function Stage({
  title,
  titleRight,
  width,
  children,
}: {
  title: ReactNode;
  titleRight?: ReactNode;
  /** 内容盒宽度，默认 400px */
  width?: number;
  children: ReactNode;
}) {
  return (
    <div className={s.stage}>
      <TitleBar title={title} titleRight={titleRight} />
      <div className={s.centerPane}>
        <div className={s.centerBox} style={width ? { width } : undefined}>
          {children}
        </div>
      </div>
    </div>
  );
}
