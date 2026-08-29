import styles from "./Kbd.module.css";
import { cx } from "../lib/cx";

/**
 * A key cap. Rendered as `<kbd>` so a screen reader says "keyboard" rather than
 * reading a glyph like `⇧` as nothing at all — hence the explicit `aria-label`
 * on any cap whose glyph does not read as a word.
 */
export function Kbd({
  children,
  label,
  className,
}: {
  children: string;
  label?: string | undefined;
  className?: string | undefined;
}) {
  return (
    <kbd
      className={cx("mono", styles.kbd, className)}
      {...(label === undefined ? {} : { "aria-label": label })}
    >
      {children}
    </kbd>
  );
}

/** A `⌘↵ open` style hint: the cap, then what it does. */
export function KeyHint({
  keys,
  label,
  action,
}: {
  keys: string;
  label?: string;
  action: string;
}) {
  return (
    <span className={styles.hint}>
      <Kbd {...(label === undefined ? {} : { label })}>{keys}</Kbd>
      {action}
    </span>
  );
}
