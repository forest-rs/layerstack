// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Independent tasks of a read, on a [`layerstack::parallel`] pool.

use alloc::vec::Vec;

/// What a task of [`run_all`] does with its item: a result.
#[cfg(feature = "std")]
pub(crate) type Each<'w, T, R> = dyn Fn(T) -> R + Sync + 'w;
/// What a task of [`run_all`] does with its item: a result.
#[cfg(not(feature = "std"))]
pub(crate) type Each<'w, T, R> = dyn Fn(T) -> R + 'w;

/// Runs `each` on every one of `items` on up to `threads` threads, and
/// returns the results in the items' order.
///
/// The pool starts a thread each time a task leaves another for it, so the
/// items are handed out as halves of halves: the first threads start while
/// the first halves are split, rather than one for each item the calling
/// thread finishes.
pub(crate) fn run_all<T: Send, R: Send>(
    threads: usize,
    items: Vec<T>,
    each: &Each<'_, T, R>,
) -> Vec<R> {
    let work = |(first, mut items): (usize, Vec<T>), added: &mut Vec<(usize, Vec<T>)>| {
        if items.len() > 1 {
            let right = items.split_off(items.len() / 2);
            added.push((first + items.len(), right));
            added.push((first, items));
            return Vec::new();
        }
        items.into_iter().map(|item| (first, each(item))).collect()
    };
    let done =
        layerstack::parallel::with_pool(threads, &work, |pool| pool.run(alloc::vec![(0, items)]));
    let mut done: Vec<(usize, R)> = done.into_iter().flatten().collect();
    done.sort_unstable_by_key(|(at, _)| *at);
    done.into_iter().map(|(_, result)| result).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn results_keep_the_items_order() {
        for threads in [1, 2, 8] {
            for len in [0, 1, 2, 3, 17, 100] {
                let items: Vec<usize> = (0..len).collect();
                let doubled = run_all(threads, items, &|n| n * 2);
                assert_eq!(doubled, (0..len).map(|n| n * 2).collect::<Vec<_>>());
            }
        }
    }
}
