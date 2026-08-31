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
 * every item has an `aria-label` *and* a `title` — the accessible name is never
 * the thing that disappears, and neither is the visible one.
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
            /*
             * The tooltip is the label when there is no label.
             *
             * Below 900px the CSS drops `.label` and six icons are all that is
             * left. `aria-label` above keeps the accessible name, so a screen
             * reader never noticed — which is exactly why the sighted case
             * stayed broken: nothing was missing from the tree, only from the
             * screen. `title` puts the name back for a pointer.
             *
             * It carries the shortcut too, because there is nowhere else to
             * put one: in Search and Files Tab is spent on the pane cycle, so
             * `⌘⌥n` is the only route to this control from those views.
             *
             * Settings names both of its keys. `⌘,` is the macOS convention
             * and the one a user tries before looking for a button (App.tsx
             * binds it), so a tooltip offering only `⌘⌥6` would be hiding the
             * binding that is actually reached for. The membership test is on
             * the view id, not on the index — the order lives in `VIEWS` and
             * a `i === 5` here would be a second copy of it.
             */
            title={
              v === "settings"
                ? `${s.label} (⌘⌥${i + 1} or ⌘,)`
                : `${s.label} (⌘⌥${i + 1})`
            }
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
