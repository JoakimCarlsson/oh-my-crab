//! What a conversation has cost.

/// How many millionths of a US dollar make a dollar: the unit costs are carried in, so that they stay whole numbers.
const MICROS_PER_DOLLAR: f64 = 1_000_000.0;

/// The US dollars that `micros` millionths of a dollar are, to show a cost.
pub fn dollars(micros: u64) -> f64 {
    micros as f64 / MICROS_PER_DOLLAR
}

/// The whole millionths of a US dollar nearest to `dollars`, to carry a cost; nothing below zero.
pub fn micros(dollars: f64) -> u64 {
    (dollars.max(0.0) * MICROS_PER_DOLLAR).round() as u64
}

/// What a conversation has cost so far, for each model it asked: the conversation's own model, the plan model, the models of its subagents, and the requests that keep the prompt cache warm.
#[derive(Clone, Debug, Default, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Spent {
    /// What each model cost, in the order they were first asked.
    #[serde(default)]
    pub models: Vec<ModelSpend>,
}

/// What the requests sent to one model cost.
#[derive(Clone, Debug, Default, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ModelSpend {
    /// The model's id; one served by another provider than the conversation's is named `provider/id`.
    pub model: String,
    /// How many requests it answered.
    #[serde(default)]
    pub requests: u64,
    /// The input tokens that were neither read from nor written to the prompt cache.
    #[serde(default)]
    pub input: u64,
    /// The tokens it wrote, reasoning included.
    #[serde(default)]
    pub output: u64,
    /// The input tokens read from the prompt cache.
    #[serde(default)]
    pub cache_read: u64,
    /// The input tokens written to the prompt cache.
    #[serde(default)]
    pub cache_write: u64,
    /// What the requests cost at the model's list prices, in millionths of a US dollar; nothing when its prices are not known.
    #[serde(default)]
    pub cost: Option<u64>,
    /// How much of `cost` was paid for cache misses, in millionths of a US dollar: what sending again the part of the conversation that an earlier request had cached cost beyond reading it from the cache.
    #[serde(default)]
    pub missed: u64,
}

impl ModelSpend {
    /// Every token the model was sent or wrote.
    pub fn tokens(&self) -> u64 {
        self.input + self.output + self.cache_read + self.cache_write
    }
}

impl Spent {
    /// What the conversation cost in millionths of a US dollar, summed over the models whose prices are known; nothing when none is.
    pub fn cost(&self) -> Option<u64> {
        self.models
            .iter()
            .filter_map(|model| model.cost)
            .reduce(|total, cost| total + cost)
    }

    /// How much of the cost was paid for cache misses, in millionths of a US dollar.
    pub fn missed(&self) -> u64 {
        self.models.iter().map(|model| model.missed).sum()
    }

    /// Every token the conversation's models were sent or wrote.
    pub fn tokens(&self) -> u64 {
        self.models.iter().map(ModelSpend::tokens).sum()
    }

    /// How many requests the conversation's models answered.
    pub fn requests(&self) -> u64 {
        self.models.iter().map(|model| model.requests).sum()
    }

    /// Whether a model with unknown prices was asked, so that the cost leaves something out.
    pub fn partial(&self) -> bool {
        self.models
            .iter()
            .any(|model| model.cost.is_none() && model.requests > 0)
    }
}

/// Tests for adding up what a conversation cost.
#[cfg(test)]
mod tests {
    use super::*;

    /// The totals sum over the models, and the cost leaves out a model without prices and says so.
    #[test]
    fn totals_sum_over_the_models() {
        let spent = Spent {
            models: vec![
                ModelSpend {
                    model: "a".to_owned(),
                    requests: 2,
                    input: 10,
                    output: 5,
                    cache_read: 100,
                    cache_write: 20,
                    cost: Some(1_500),
                    missed: 300,
                },
                ModelSpend {
                    model: "b".to_owned(),
                    requests: 1,
                    input: 7,
                    ..ModelSpend::default()
                },
            ],
        };
        assert_eq!(spent.cost(), Some(1_500));
        assert_eq!(spent.missed(), 300);
        assert_eq!(spent.tokens(), 142);
        assert_eq!(spent.requests(), 3);
        assert!(spent.partial());
        assert_eq!(Spent::default().cost(), None);
        assert!(!Spent::default().partial());
        assert_eq!(dollars(1_500), 0.0015);
        assert_eq!(micros(0.0015), 1_500);
        assert_eq!(micros(-1.0), 0);
    }

    /// A tally written before a field existed still reads.
    #[test]
    fn missing_fields_read_as_nothing() {
        let spent: Spent = serde_json::from_str(r#"{"models":[{"model":"a"}]}"#).unwrap();
        assert_eq!(spent.models[0].cost, None);
        assert_eq!(spent.requests(), 0);
    }
}
