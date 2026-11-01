// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

/** One titled section of the inspector, with an optional note at the title's end. */
export function Section({ title, children, aside }: { title: string; children: React.ReactNode; aside?: React.ReactNode }) {
  return (
    <section className="border-b px-3 py-2">
      <h3 className="mb-1.5 flex items-center text-[11px] font-medium tracking-wide text-muted-foreground uppercase">
        {title}
        <span className="ml-auto normal-case">{aside}</span>
      </h3>
      {children}
    </section>
  );
}
