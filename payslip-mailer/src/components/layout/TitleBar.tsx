import type { ReactNode } from "react";
import { isTauri } from "@tauri-apps/api/core";
import s from "./TitleBar.module.css";

/* ============================================================
   窗口标题栏 · 全应用唯一的一处
   S03 工作台、S01 初始化向导、S02 锁屏共用它：
   同一个窗口外观，只有下面的内容区不同（设计文档 ch.11 布局与栅格）。
   ============================================================ */

export function TitleBar({
  title,
  titleRight,
}: {
  title: ReactNode;
  titleRight?: ReactNode;
}) {
  return (
    <div className={s.titlebar}>
      {/* 在 Tauri 内使用系统原生的红绿灯（Overlay 样式浮在标题栏上）；
          浏览器预览时画一组仿制的，避免出现两套 */}
      {isTauri() ? (
        <span className={s.titlebarLeftPad} aria-hidden />
      ) : (
        <span className={s.lights} aria-hidden>
          <i />
          <i />
          <i />
        </span>
      )}
      <span className={s.title}>{title}</span>
      <span className={s.titleRight}>{titleRight}</span>
    </div>
  );
}
