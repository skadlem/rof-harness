//! Per-endpoint capability: the prompt ceiling above which an endpoint stops
//! emitting content, plus the order the retry ladder climbs. The shipped
//! default reproduces the DeepSeek/vLLM behavior bit for bit; another
//! endpoint overrides the numbers, not the code.

use serde::{Deserialize, Serialize};

/// One retry-ladder rung, in the vocabulary the request already speaks: each
/// rung sets a flag on `LlmReq` (roomier also doubles the budget, shrink
/// halves it). Kept distinct because endpoints honour different knobs at
/// different prompt sizes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum LadderRung {
    ReasoningLow,
    ThinkingOff,
    ReasoningOff,
    Roomier,
    Shrink,
}

/// Per-endpoint capability. Defaults reproduce the shipped DeepSeek/vLLM
/// behavior bit for bit (see the order test).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct EndpointProfile {
    pub emission_threshold_chars: usize,
    pub ladder: Vec<LadderRung>,
}

impl Default for EndpointProfile {
    fn default() -> Self {
        Self {
            emission_threshold_chars: 12_000,
            ladder: vec![
                LadderRung::ReasoningLow,
                LadderRung::ThinkingOff,
                LadderRung::ReasoningOff,
                LadderRung::Roomier,
                LadderRung::Shrink,
            ],
        }
    }
}

/// The truncation ladder as a walk over the profile's rung order: the first
/// rung whose guard holds fires. Skip-if-set makes the walk order-independent
/// for custom profiles; the default order reproduces `reshape_for_truncation`
/// exactly (order is the point: cheapest-quality rungs first, roomier only
/// when content shipped, shrink terminal on the zero-content sequence).
/// Returns whether a reshape was applied.
pub fn apply_ladder(
    req: &mut crate::llm::LlmReq,
    content_chars: usize,
    profile: &EndpointProfile,
) -> bool {
    for rung in &profile.ladder {
        match rung {
            LadderRung::ReasoningLow => {
                if !req.reasoning_low && !req.thinking_off && !req.reasoning_off {
                    req.reasoning_low = true;
                    return true;
                }
            }
            LadderRung::ThinkingOff => {
                if !req.thinking_off && !req.reasoning_off {
                    req.thinking_off = true;
                    return true;
                }
            }
            LadderRung::ReasoningOff => {
                if !req.reasoning_off {
                    // Exclusive: gateways reject contradictory knob combos
                    // (measured 400/422 for low+thinking_off+reasoning_off
                    // together), so the strongest rung travels alone. Weaker
                    // pairs (low / low+thinking_off) keep accumulating —
                    // those shapes are validated, this one was not.
                    req.reasoning_off = true;
                    req.reasoning_low = false;
                    req.thinking_off = false;
                    return true;
                }
            }
            LadderRung::Roomier => {
                if !req.roomier && content_chars > 0 {
                    req.roomier = true;
                    req.max_tokens = req.max_tokens.saturating_mul(2);
                    req.reasoning_low = false;
                    req.thinking_off = false;
                    req.reasoning_off = false;
                    return true;
                }
            }
            LadderRung::Shrink => {
                if content_chars == 0 && !req.shrunk && req.max_tokens > 1024 {
                    req.shrunk = true;
                    req.max_tokens = (req.max_tokens / 2).max(1024);
                    req.reasoning_low = false;
                    req.thinking_off = false;
                    return true;
                }
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::{apply_ladder, EndpointProfile, LadderRung};
    use crate::llm::LlmReq;

    fn blank_req(max_tokens: usize) -> LlmReq {
        LlmReq {
            system: String::new(),
            prompt: String::new(),
            max_tokens,
            reasoning_off: false,
            reasoning_low: false,
            roomier: false,
            shrunk: false,
            thinking_off: false,
        }
    }

    #[test]
    fn default_profile_reproduces_the_shipped_ladder_order() {
        assert_eq!(
            EndpointProfile::default().ladder,
            vec![
                LadderRung::ReasoningLow,
                LadderRung::ThinkingOff,
                LadderRung::ReasoningOff,
                LadderRung::Roomier,
                LadderRung::Shrink,
            ]
        );
        assert_eq!(EndpointProfile::default().emission_threshold_chars, 12_000);
    }

    #[test]
    fn a_custom_order_is_honoured() {
        let p = EndpointProfile {
            emission_threshold_chars: 12_000,
            ladder: vec![LadderRung::ReasoningOff, LadderRung::ReasoningLow],
        };
        let mut req = blank_req(8000);
        assert!(apply_ladder(&mut req, 100, &p));
        assert!(req.reasoning_off && !req.reasoning_low);
    }

    #[test]
    fn an_empty_ladder_is_spent() {
        let p = EndpointProfile {
            emission_threshold_chars: 12_000,
            ladder: vec![],
        };
        let mut req = blank_req(8000);
        assert!(!apply_ladder(&mut req, 100, &p));
    }

    #[test]
    fn reasoning_off_clears_weaker_knobs() {
        // Gateways reject contradictory knob combos (measured 400/422 on Go
        // for low+thinking_off+reasoning_off together): the strongest rung
        // is exclusive, so its wire shape carries exactly one knob.
        let p = EndpointProfile::default();
        let mut req = blank_req(8000);
        req.reasoning_low = true;
        req.thinking_off = true;
        assert!(apply_ladder(&mut req, 0, &p));
        assert!(req.reasoning_off);
        assert!(!req.reasoning_low && !req.thinking_off);
    }

    #[test]
    fn shrink_needs_zero_content_and_room_above_the_floor() {
        let p = EndpointProfile::default();
        let mut req = blank_req(8000);
        req.reasoning_off = true;
        // Content shipped: roomier (not shrink) answers.
        assert!(apply_ladder(&mut req, 100, &p));
        assert!(!req.shrunk && req.roomier);
        // Zero content: shrink fires and halves the budget.
        let mut req = blank_req(8000);
        req.reasoning_off = true;
        assert!(apply_ladder(&mut req, 0, &p));
        assert!(req.shrunk);
        assert_eq!(req.max_tokens, 4000);
    }
}
