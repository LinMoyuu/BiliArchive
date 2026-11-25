//! Bounded, ordered worker batches for local archive input.
pub fn workers() -> usize {
    std::thread::available_parallelism().map_or(1, |count| count.get().min(4))
}

pub fn map_ordered<T, R, F>(items: &[T], workers: usize, read: F) -> Vec<R>
where
    T: Sync,
    R: Send,
    F: Fn(&T) -> R + Sync,
{
    if workers <= 1 || items.len() < 2 {
        return items.iter().map(read).collect();
    }
    let chunk_size = items.len().div_ceil(workers);
    std::thread::scope(|scope| {
        let jobs: Vec<_> = items
            .chunks(chunk_size)
            .map(|chunk| {
                let read = &read;
                scope.spawn(move || chunk.iter().map(read).collect::<Vec<_>>())
            })
            .collect();
        jobs.into_iter()
            .flat_map(|job| job.join().expect("archive reader panicked"))
            .collect()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parallel_reads_preserve_input_order() {
        let input: Vec<_> = (0..127).collect();
        assert_eq!(
            map_ordered(&input, 1, |n| n * 2),
            map_ordered(&input, 4, |n| n * 2)
        );
    }
}
