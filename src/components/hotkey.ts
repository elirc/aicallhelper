/**
 * The registration status the bridge reports for the global shortcut.
 * Kept in components/ so both the views and the leaf components can import it
 * without reaching upward into App.
 */
export interface HotkeyStatus {
  accelerator: string;
  registered: boolean;
}
