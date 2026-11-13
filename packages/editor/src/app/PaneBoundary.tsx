// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// One of the editor's panes (the scene tabs, the viewport, the inspector,
// the bottom tabs), or, when something in it throws while rendering, a
// note naming it and why, with a way to draw it again. Without it React
// unmounts the whole editor for one panel's fault: a blank page, and the
// unsaved edits out of reach. The inspector's sections have their own,
// smaller boundary (`Inspector.tsx`), so one view's failure leaves the
// others.
import { Component, type ErrorInfo, type ReactNode } from "react";
import { Button } from "@/components/ui/button";

export class PaneBoundary extends Component<{ title: string; children: ReactNode }, { error: string | null }> {
  override state = { error: null as string | null };

  static getDerivedStateFromError(error: unknown) {
    return { error: error instanceof Error ? error.message : String(error) };
  }

  override componentDidCatch(error: unknown, info: ErrorInfo) {
    console.error(`the ${this.props.title} failed`, error, info.componentStack);
  }

  override render() {
    if (this.state.error === null) return this.props.children;
    return (
      <div role="alert" className="flex h-full flex-col items-start gap-2 p-3 text-xs">
        <p className="text-destructive">
          The {this.props.title} failed: {this.state.error}
        </p>
        <p className="text-muted-foreground">The stage and its unsaved edits are as they were; the rest of the editor still works.</p>
        <Button variant="outline" size="xs" onPress={() => this.setState({ error: null })}>
          Show it again
        </Button>
      </div>
    );
  }
}
