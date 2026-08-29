import styles from "./ErrorNotice.module.css";
import { cx } from "../lib/cx";
import { Icon } from "./Icon";
import { Code, type UiError } from "../api";

/**
 * A failure, rendered.
 *
 * Two rules, both from the backend contract:
 *
 *   • Branch on `code`, never on the message text. A cloud-only file
 *     (`FS_PLACEHOLDER_SKIPPED`) needs a different affordance from a parse
 *     failure, and prose is not a contract.
 *   • Render `message` as-is. It already names a cause and an action
 *     (UX principle 4); paraphrasing it can only lose information.
 */
export interface ErrorAction {
  label: string;
  onClick: () => void;
}

/** Codes that mean "not broken, just not here" — a warning, not a failure. */
const EXPECTED = new Set<string>([
  Code.PlaceholderSkipped,
  Code.NotFound,
]);

export function ErrorNotice({
  error,
  action,
  compact,
}: {
  error: UiError;
  action: ErrorAction | null;
  compact?: boolean;
}) {
  const expected = EXPECTED.has(error.code);
  return (
    <div
      className={cx(
        styles.notice,
        expected ? styles.warn : styles.error,
        compact && styles.compact,
      )}
      role="status"
    >
      <Icon name="warning" size={14} className={styles.icon} />
      <div className={styles.body}>
        <p className={styles.message}>{error.message}</p>
        <p className={cx("mono", styles.code)}>{error.code}</p>
      </div>
      {action && (
        <button type="button" className={styles.action} onClick={action.onClick}>
          {action.label}
        </button>
      )}
    </div>
  );
}
