//! 预算控制（`docs/01` §7.8.5）。
//!
//! M5 粒度：**每次 ai.run 作业**一个 Budget；模型调用次数耗尽即停。

#[derive(Debug, Clone)]
pub struct Budget {
    pub max_calls: u32,
    pub used: u32,
}

impl Budget {
    pub fn new(max_calls: u32) -> Self {
        Self {
            max_calls,
            used: 0,
        }
    }

    /// 尝试记一次调用；耗尽返回 false。
    pub fn try_spend(&mut self) -> bool {
        if self.used >= self.max_calls {
            false
        } else {
            self.used += 1;
            true
        }
    }

    pub fn remaining(&self) -> u32 {
        self.max_calls.saturating_sub(self.used)
    }

    pub fn exhausted(&self) -> bool {
        self.used >= self.max_calls
    }
}

/// 默认预算（防误配大作业烧钱；`docs/01` §7.8.5）。
pub const DEFAULT_MAX_CALLS: u32 = 50;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn budget_exhaustion() {
        let mut b = Budget::new(2);
        assert!(b.try_spend());
        assert!(b.try_spend());
        assert!(!b.try_spend());
        assert!(b.exhausted());
        assert_eq!(b.remaining(), 0);
    }
}
