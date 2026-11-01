// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

/** A byte count in B, KB, MB or GB. */
export function formatBytes(count: number): string {
  if (count < 1024) return `${count} B`;
  if (count < 1024 * 1024) return `${(count / 1024).toFixed(1)} KB`;
  if (count < 1024 * 1024 * 1024) return `${(count / (1024 * 1024)).toFixed(1)} MB`;
  return `${(count / (1024 * 1024 * 1024)).toFixed(2)} GB`;
}

/** A layer's name as the editor shows it. */
export function layerName(identifier: string | null): string {
  return identifier ?? "(anonymous)";
}
