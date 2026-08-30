import styles from "./TitleBar.module.css";
import { Icon } from "./Icon";
import { useUi } from "../store";

/**
 * The window's drag strip, and the two controls that belong on it.
 *
 * `tauri.conf.json` sets `titleBarStyle: "Overlay"` with `hiddenTitle`, so macOS
 * draws the traffic lights over the top-left of the WebView and draws nothing
 * else. This bar reserves that corner and provides the drag region — the only
 * window capability the manifest grants is `core:window:allow-start-dragging`,
 * which is what `data-tauri-drag-region` uses.
 *
 * It used to carry the name of the current view and nothing else, which the
 * section switcher now says in the same word. What it carries instead is the
 * switcher itself and the rail toggle: both are chrome, chrome belongs on the
 * canvas, and putting navigation here is what lets the sidebar be collapsed
 * without losing every way out of the view you are in.
 *
 * The drag region is on the strip, not on its children: a button inside a drag
 * region is still a button, but a button that *is* one swallows its own click
 * into a window move.
 */
export function TitleBar({ children }: { children?: React.ReactNode }) {
  const collapsed = useUi((s) => s.sidebarCollapsed);
  const toggle = useUi((s) => s.toggleSidebar);

  return (
    <header className={styles.bar} data-tauri-drag-region>
      <div className={styles.lights} aria-hidden="true" />
      <button
        type="button"
        className={styles.rail}
        aria-label={collapsed ? "Show the sidebar" : "Hide the sidebar"}
        aria-expanded={!collapsed}
        title={collapsed ? "Show the sidebar (⌘\\)" : "Hide the sidebar (⌘\\)"}
        onClick={toggle}
      >
        <Icon name="sidebar" size={14} />
      </button>
      <div className={styles.centre} data-tauri-drag-region>
        {children}
      </div>
      <div className={styles.lights} aria-hidden="true" />
    </header>
  );
}
