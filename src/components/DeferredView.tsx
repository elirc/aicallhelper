import { Component, type ReactNode } from 'react';

/** A failed optional chunk must not take down an active session. */
export class DeferredView extends Component<
  { children: ReactNode; onClose(): void },
  { failed: boolean }
> {
  state = { failed: false };
  static getDerivedStateFromError() { return { failed: true }; }

  render() {
    if (this.state.failed) {
      return (
        <div className="panel panel-body">
          <p role="alert">This view could not load. Close it and restart the app when your session is finished.</p>
          <button type="button" className="ghost-button" onClick={this.props.onClose}>Back to assistant</button>
        </div>
      );
    }
    return this.props.children;
  }
}
