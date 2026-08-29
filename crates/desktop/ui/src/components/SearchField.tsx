import { forwardRef } from "react";

import styles from "./SearchField.module.css";
import { cx } from "../lib/cx";
import { Icon } from "./Icon";
import { Kbd } from "./Kbd";

export interface SearchFieldProps {
  value: string;
  onChange: (v: string) => void;
  placeholder: string;
  /** Shown as a cap on the right, like the mockup's `⌘F`. */
  cap?: string;
  size?: "compact" | "wide";
  label: string;
}

/**
 * The search field. Uncontrolled debouncing lives in the caller, not here: the
 * field must repaint on the same frame as the keystroke (GUI §7, < 16 ms), so
 * its value is always exactly what was typed.
 */
export const SearchField = forwardRef<HTMLInputElement, SearchFieldProps>(
  function SearchField(
    { value, onChange, placeholder, cap, size = "compact", label },
    ref,
  ) {
    return (
      <div className={cx(styles.field, size === "wide" && styles.wide)}>
        <Icon name="search" size={size === "wide" ? 17 : 14} className={styles.icon} />
        <input
          ref={ref}
          className={styles.input}
          type="text"
          value={value}
          onChange={(e) => onChange(e.target.value)}
          placeholder={placeholder}
          aria-label={label}
          autoComplete="off"
          autoCorrect="off"
          autoCapitalize="off"
          spellCheck={false}
        />
        {cap !== undefined && value === "" && <Kbd>{cap}</Kbd>}
      </div>
    );
  },
);
