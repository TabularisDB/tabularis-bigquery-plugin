import type { CSSProperties } from "react";

export const PLUGIN_ID = "bigquery";

export const styles = {
  container: {
    display: "flex",
    flexDirection: "column",
    gap: "12px",
  } as CSSProperties,
  notice: {
    border: "1px solid color-mix(in srgb, #4285f4 35%, transparent)",
    borderRadius: "8px",
    background: "color-mix(in srgb, #4285f4 8%, transparent)",
    color: "var(--color-text-secondary, #cbd5e1)",
    fontSize: "12px",
    lineHeight: 1.5,
    padding: "10px 12px",
  } as CSSProperties,
  field: {
    display: "flex",
    flexDirection: "column",
    gap: "4px",
  } as CSSProperties,
  label: {
    color: "var(--color-text-muted, #94a3b8)",
    fontSize: "10px",
    fontWeight: 600,
    letterSpacing: "0.06em",
    textTransform: "uppercase",
  } as CSSProperties,
  input: {
    width: "100%",
    boxSizing: "border-box",
    padding: "8px 12px",
    borderRadius: "6px",
    border: "1px solid var(--color-border-strong, #475569)",
    background: "var(--color-bg-base, transparent)",
    color: "var(--color-text-primary, inherit)",
    fontSize: "13px",
  } as CSSProperties,
  textarea: {
    resize: "vertical",
  } as CSSProperties,
  help: {
    color: "var(--color-text-muted, #94a3b8)",
    fontSize: "11px",
    lineHeight: 1.5,
    margin: "-6px 0 0",
  } as CSSProperties,
  status: {
    color: "var(--color-text-muted, #94a3b8)",
    fontSize: "11px",
    lineHeight: 1.5,
  } as CSSProperties,
  clearButton: {
    alignSelf: "flex-start",
    border: 0,
    background: "transparent",
    color: "var(--color-text-danger, #f87171)",
    cursor: "pointer",
    fontSize: "11px",
    padding: 0,
  } as CSSProperties,
  warning: {
    color: "var(--color-text-danger, #f87171)",
    fontSize: "11px",
    lineHeight: 1.5,
    margin: "-6px 0 0",
  } as CSSProperties,
  details: {
    border: "1px solid var(--color-border-strong, #475569)",
    borderRadius: "8px",
    padding: "8px 12px",
  } as CSSProperties,
  summary: {
    color: "var(--color-text-muted, #94a3b8)",
    cursor: "pointer",
    fontSize: "11px",
    fontWeight: 600,
    letterSpacing: "0.06em",
    textTransform: "uppercase",
    marginBottom: "4px",
  } as CSSProperties,
  grid: {
    display: "grid",
    gridTemplateColumns: "1fr 1fr",
    gap: "12px",
  } as CSSProperties,
};
