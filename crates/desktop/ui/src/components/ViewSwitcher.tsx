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
import { useUi, type View } from "../store";

const SECTIONS: ReadonlyArray<{ view: View; label: string; icon: IconName }> = [
  { view: "search", label: "Search", icon: "search" },
  { view: "ask", label: "Ask", icon: "ask" },
  { view: "files", label: "Files", icon: "file" },
  { view: "models", label: "Models", icon: "chip" },
  { view: "status", label: "Status", icon: "activity" },
  { view: "settings", label: "Settings", icon: "settings" },
];

export function ViewSwitcher() {
  const view = useUi((s) => s.view);
  const setView = useUi((s) => s.setView);

  return (
    <nav className={styles.switcher} aria-label="Sections">
      {SECTIONS.map((s) => (
        <button
          key={s.view}
          type="button"
          className={cx(styles.item, view === s.view && styles.active)}
          aria-current={view === s.view ? "page" : undefined}
          aria-label={s.label}
          title={s.label}
          onClick={() => setView(s.view)}
        >
          <Icon name={s.icon} size={13} />
          <span className={styles.label} aria-hidden="true">
            {s.label}
          </span>
        </button>
      ))}
    </nav>
  );
}
