/**
 * The six sections, as one control in the title strip.
 *
 * They used to be a stack of full-width rows at the top of the sidebar, holding
 * 176px of permanent width for six destinations — four of which are visited
 * about once a week. That is an IDE's shape. A chat product spends its one
 * permanent column on *content*, which here is the conversation list, so the
 * sections moved to the strip that was already there and was carrying nothing
 * but a word.
 *
 * Two consequences worth stating, because both are improvements rather than
 * side effects:
 *
 *  - navigation now survives the sidebar being collapsed, which it did not:
 *    hiding the rail used to hide every way to leave the view you were in
 *    except the keyboard;
 *  - the strip already said which section you were in, in the same word this
 *    control marks, so nothing was lost by replacing it.
 *
 * Labels drop out on a narrow window and the icons carry it alone. That is why
 * every item has an `aria-label` regardless — the accessible name is never the
 * thing that disappears.
 */

import styles from "./ViewSwitcher.module.css";
import { cx } from "../lib/cx";
import { Icon, type IconName } from "./Icon";
import { useUi, VIEWS, type View } from "../store";

/**
 * What each section is called and what it is drawn as.
 *
 * The *order* is not here — it is `VIEWS` in the store, which is also what the
 * `⌘⌥n` keys count through. Two lists would let the third button and `⌘⌥3`
 * drift apart, and nothing would have failed when they did.
 */
const SECTION: Record<View, { label: string; icon: IconName }> = {
  search: { label: "Search", icon: "search" },
  ask: { label: "Ask", icon: "ask" },
  files: { label: "Files", icon: "file" },
  models: { label: "Models", icon: "chip" },
  status: { label: "Status", icon: "activity" },
  settings: { label: "Settings", icon: "settings" },
};

export function ViewSwitcher() {
  const view = useUi((s) => s.view);
  const setView = useUi((s) => s.setView);

  return (
    /*
     * `data-switcher` is read by `App`'s focus effect, which moves focus off
     * this control and into the view it just opened. A marker attribute rather
     * than a class name because the class is a CSS-module hash and matching on
     * one is matching on a build artifact.
     */
    <nav className={styles.switcher} aria-label="Sections" data-switcher="">
      {VIEWS.map((v, i) => {
        const s = SECTION[v];
        return (
          <button
            key={v}
            type="button"
            className={cx(styles.item, view === v && styles.active)}
            aria-current={view === v ? "page" : undefined}
            aria-label={s.label}
            // The shortcut belongs on the tooltip: this control's labels drop
            // out on a narrow window, and in Search and Files Tab is spent on
            // the pane cycle, so `⌘⌥n` is how these are reached from there.
            title={`${s.label} (⌘⌥${i + 1})`}
            onClick={() => setView(v)}
          >
            <Icon name={s.icon} size={13} />
            <span className={styles.label} aria-hidden="true">
              {s.label}
            </span>
          </button>
        );
      })}
    </nav>
  );
}
