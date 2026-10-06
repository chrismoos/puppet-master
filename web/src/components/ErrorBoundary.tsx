import { Component, type ReactNode } from "react";

// Contains a rendering or effect crash to the pane it happened in, so
// one broken view never takes down the whole dashboard. Remounts (and
// so retries) when its key changes, which the app ties to the route.

export class ErrorBoundary extends Component<{ children: ReactNode; resetKey?: string | number }, { error: Error | null }> {
  state = { error: null as Error | null };

  static getDerivedStateFromError(error: Error): { error: Error } {
    return { error };
  }

  componentDidCatch(error: Error): void {
    console.error("pane crashed", error);
  }

  componentDidUpdate(prevProps: { resetKey?: string | number }): void {
    if (this.state.error && prevProps.resetKey !== this.props.resetKey) {
      this.setState({ error: null });
    }
  }

  render(): ReactNode {
    if (this.state.error) {
      return (
        <div className="pane-empty" role="alert">
          <p className="pane-empty-title">this view crashed</p>
          <p className="muted-line">{this.state.error.message}</p>
          <button
            type="button"
            className="btn"
            onClick={() => this.setState({ error: null })}
          >
            try again
          </button>
        </div>
      );
    }
    return this.props.children;
  }
}
