use crate::config::LevelConfig;

/// Cumulative XP needed to reach `level` (level 1 starts at 0).
pub fn threshold(level: u32, cfg: &LevelConfig) -> u64 {
    if level <= 1 {
        return 0;
    }
    (cfg.base * ((level - 1) as f64).powf(cfg.exponent)).round() as u64
}

/// Level for a total XP amount.
pub fn level_for(total: u64, cfg: &LevelConfig) -> u32 {
    let mut level = 1;
    while level < u32::MAX - 1 && threshold(level + 1, cfg) <= total {
        level += 1;
    }
    level
}

/// (level, xp into level, xp span of level)
pub fn progress(total: u64, cfg: &LevelConfig) -> (u32, u64, u64) {
    let level = level_for(total, cfg);
    let lo = threshold(level, cfg);
    let hi = threshold(level + 1, cfg);
    (level, total - lo, hi - lo)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn curve_values() {
        let c = LevelConfig::default();
        assert_eq!(threshold(1, &c), 0);
        assert_eq!(threshold(2, &c), 100);
        assert_eq!(threshold(3, &c), 283);
        assert_eq!(threshold(5, &c), 800);
        assert_eq!(level_for(0, &c), 1);
        assert_eq!(level_for(99, &c), 1);
        assert_eq!(level_for(100, &c), 2);
        assert_eq!(level_for(282, &c), 2);
        assert_eq!(level_for(283, &c), 3);
    }

    #[test]
    fn progress_values() {
        let c = LevelConfig::default();
        assert_eq!(progress(150, &c), (2, 50, 183));
    }
}
