//! Bounded, ordered worker batches for local archive input.
pub const MAX_WORKERS: usize = 16;

pub fn workers() -> usize {
    std::thread::available_parallelism().map_or(1, |count| count.get().min(4))
}

pub fn parse_workers(text: &str) -> Result<Option<usize>, &'static str> {
    let text = text.trim();
    if text.is_empty() {
        return Ok(None);
    }
    text.parse::<usize>()
        .ok()
        .filter(|count| (1..=MAX_WORKERS).contains(count))
        .map(Some)
        .ok_or("本地归档线程数须为 1–16 的整数，留空为自动")
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
    fn worker_setting_validates_auto_and_bounds() {
        assert_eq!(parse_workers(" "), Ok(None));
        assert_eq!(parse_workers(" 1 "), Ok(Some(1)));
        assert_eq!(parse_workers("16"), Ok(Some(16)));
        for invalid in ["0", "17", "-1", "2.5", "text"] {
            assert!(parse_workers(invalid).is_err(), "{invalid}");
        }
    }

    #[test]
    fn parallel_reads_preserve_input_order() {
        let input: Vec<_> = (0..127).collect();
        assert_eq!(
            map_ordered(&input, 1, |n| n * 2),
            map_ordered(&input, 4, |n| n * 2)
        );
    }
}
