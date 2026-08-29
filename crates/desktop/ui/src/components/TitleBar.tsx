import styles from "./TitleBar.module.css";

/**
 * The window's drag strip.
 *
 * `tauri.conf.json` sets `titleBarStyle: "Overlay"` with `hiddenTitle`, so macOS
 * draws the traffic lights over the top-left of the WebView and draws nothing
 * else. This bar reserves that corner and provides the drag region — the only
 * window capability the manifest grants is `core:window:allow-start-dragging`,
 * which is what `data-tauri-drag-region` uses.
 */
export function TitleBar({ title }: { title: string }) {
  return (
    <header className={styles.bar} data-tauri-drag-region>
      <div className={styles.lights} aria-hidden="true" />
      <div className={styles.title} data-tauri-drag-region>
        {title}
      </div>
      <div className={styles.lights} aria-hidden="true" />
    </header>
  );
}
