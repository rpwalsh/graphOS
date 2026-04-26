// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! Digital twin prediction engine — predictive computing for system state.
//!
//! This module implements a digital twin of the system's hardware state
//! embedded directly in the kernel's graph substrate, enabling
//! spectral-temporal prediction of future system states.
//!
//! ## Architecture overview
//!
//! The digital twin models hardware subsystems (CPU cores, thermal zones,
//! power rails, clock domains, memory controllers) as graph nodes with
//! typed edges carrying real-time telemetry. The existing GraphOS
//! spectral analysis (eigenvalue tracking, CUSUM) and causal inference
//! (Granger/transfer entropy) operate *directly* on this hardware twin
//! graph to produce predictive signals:
//!
//! 1. **Telemetry ingestion**: Hardware metrics are written to twin nodes
//!    as timestamped observations (ring buffer per sensor).
//! 2. **Spectral prediction**: The Fiedler value of the twin subgraph
//!    predicts thermal throttling events (Fiedler drift → thermal cascade).
//! 3. **Causal tracing**: Granger causality over telemetry time series
//!    identifies root-cause chains (e.g., CPU load → thermal → DVFS).
//! 4. **Predictive scheduling**: The scheduler consults twin predictions
//!    to migrate tasks before thermal events occur.
//!
//! ## Probabilistic state model
//!
//! - **State distributions**: Each twin node maintains a probability
//!   distribution (histogram) over future states, collapsed to a point
//!   estimate only when the scheduler queries it.
//! - **Causal links**: Dependencies between twin nodes propagate state
//!   changes — updating CPU load shifts the thermal zone's predicted
//!   distribution proportional to coupling strength and source deviation.
//! - **Spectral coherence**: The eigenvalue spectrum of the twin subgraph
//!   measures system stability — stable eigenvalues indicate coherent
//!   system state; drift indicates impending anomaly.
//!
//! ## Integration with spectral + causal subsystems
//!
//! - `spectral::record_snapshot()` feeds Fiedler values to twin drift tracking.
//! - `causal::CausalGraph` discoveries update entanglement coupling strengths.
//! - The cognitive pipeline's Perceive stage queries twin predictions
//!   as evidence items.
//!
//! ## Design
//! - No heap. All state is statically allocated.
//! - All values are 16.16 fixed-point (same as graph weights).
//! - Ring buffers for time series, fixed-size histogram for distributions.
//! - Twin nodes reuse existing `NodeKind::Device` and `NodeKind::CpuCore`.
//! - Prediction error is tracked per-sensor (EMA of |actual − predicted|)
//!   and automatically degrades confidence when the model is wrong.

use crate::graph::types::*;

// ────────────────────────────────────────────────────────────────────
// Constants
// ────────────────────────────────────────────────────────────────────

/// Maximum number of twin sensor nodes.
pub const MAX_TWIN_SENSORS: usize = 32;

/// Time-series ring buffer depth per sensor.
pub const SENSOR_HISTORY: usize = 128;

/// Histogram bins for state distribution (the "superposition" model).
pub const HIST_BINS: usize = 16;

/// Prediction horizon: how many steps ahead we forecast.
pub const PREDICTION_HORIZON: usize = 8;

/// Exponential moving average decay for telemetry smoothing (16.16).
/// alpha = 0.1 → 6554 in 16.16.
const EMA_ALPHA: Weight = 6554;

/// Threshold for Fiedler drift alarm (16.16). -0.1 sustained → alarm.
const FIEDLER_DRIFT_THRESHOLD: i32 = -6554; // -0.1 in 16.16 as signed

/// Number of consecutive drift steps to trigger prediction alarm.
const DRIFT_SUSTAIN_STEPS: u32 = 5;

/// Prediction error EMA alpha (16.16). Higher = faster adaptation to errors.
/// 0.2 → 13107 in 16.16.
const ERROR_ALPHA: Weight = 13107;

/// Confidence penalty per unit of prediction error (16.16).
/// When error_ema reaches 50% of sensor range, confidence is halved.
const ERROR_CONFIDENCE_SCALE: Weight = WEIGHT_ONE;

// ────────────────────────────────────────────────────────────────────
// Sensor kind
// ────────────────────────────────────────────────────────────────────

/// Classification of hardware sensor being twinned.
///
/// Drives which causal model and prediction strategy applies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum SensorKind {
    /// CPU core temperature (millidegrees C in raw value).
    CpuTemperature = 0,
    /// GPU temperature.
    GpuTemperature = 1,
    /// SoC/package temperature.
    SocTemperature = 2,
    /// CPU core utilization (0–65536 = 0–100% in 16.16).
    CpuUtilization = 3,
    /// Memory utilization.
    MemoryUtilization = 4,
    /// Power rail voltage (millivolts in raw).
    Voltage = 5,
    /// Power consumption (milliwatts in raw).
    PowerDraw = 6,
    /// Clock frequency (MHz in raw).
    ClockFrequency = 7,
    /// Interrupt rate (interrupts per second).
    InterruptRate = 8,
    /// Cache hit rate (0–65536 = 0–100% in 16.16).
    CacheHitRate = 9,
    /// Memory bandwidth utilization.
    MemoryBandwidth = 10,
    /// IPC throughput (messages per second).
    IpcThroughput = 11,
    /// Display refresh cadence (presents per second).
    DisplayRefreshRate = 12,
    /// Display write bandwidth in MiB/s.
    DisplayBandwidth = 13,
    /// Fraction of the visible scanout touched by a write.
    DisplayCoverage = 14,
}

// ────────────────────────────────────────────────────────────────────
// Sensor observation
// ────────────────────────────────────────────────────────────────────

/// A single timestamped telemetry observation.
///
/// 16 bytes, repr(C), Copy.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct Observation {
    /// Boot-relative timestamp.
    pub timestamp: Timestamp,
    /// Observed value in 16.16 fixed-point.
    pub value: Weight,
    /// EMA-smoothed value at this point.
    pub smoothed: Weight,
    /// Reserved.
    pub _pad: u32,
}

impl Observation {
    pub const EMPTY: Self = Self {
        timestamp: 0,
        value: 0,
        smoothed: 0,
        _pad: 0,
    };
}

// ────────────────────────────────────────────────────────────────────
// State distribution — probabilistic future-state model
// ────────────────────────────────────────────────────────────────────

/// Probability distribution over discretized sensor values.
///
/// Each bin covers an equal range of the sensor's [min, max] domain.
/// Counts represent unnormalized frequency. The "collapse" operation
/// extracts the expected value (weighted mean).
///
/// The twin holds uncertainty about the sensor's future state until
/// the scheduler queries it, at which point the distribution is
/// collapsed to a point estimate.
///
/// 80 bytes, repr(C), Copy.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct StateDistribution {
    /// Lower bound of the sensor's domain (16.16).
    domain_min: Weight,
    /// Upper bound of the sensor's domain (16.16).
    domain_max: Weight,
    /// Bin counts (unnormalized). Sum of all bins = total observations.
    bins: [u32; HIST_BINS],
    /// Total observations (for normalization).
    total: u32,
    /// Padding.
    _pad: u32,
}

impl StateDistribution {
    pub const fn empty(domain_min: Weight, domain_max: Weight) -> Self {
        Self {
            domain_min,
            domain_max,
            bins: [0; HIST_BINS],
            total: 0,
            _pad: 0,
        }
    }

    /// Domain lower bound (16.16).
    pub fn domain_min(&self) -> Weight {
        self.domain_min
    }
    /// Domain upper bound (16.16).
    pub fn domain_max(&self) -> Weight {
        self.domain_max
    }
    /// Total observations recorded.
    pub fn total(&self) -> u32 {
        self.total
    }

    /// Record an observation into the histogram.
    pub fn record(&mut self, value: Weight) {
        if self.domain_max <= self.domain_min {
            return;
        }
        let range = self.domain_max - self.domain_min;
        let offset = if value < self.domain_min {
            0
        } else if value >= self.domain_max {
            HIST_BINS - 1
        } else {
            let shifted = value - self.domain_min;
            // bin = shifted * HIST_BINS / range, integer arithmetic
            let bin = ((shifted as u64 * HIST_BINS as u64) / range as u64) as usize;
            if bin >= HIST_BINS { HIST_BINS - 1 } else { bin }
        };
        self.bins[offset] = self.bins[offset].saturating_add(1);
        self.total = self.total.saturating_add(1);
    }

    /// Compute the expected value from the distribution (16.16).
    ///
    /// Returns the probability-weighted mean of bin centers.
    pub fn collapse(&self) -> Weight {
        if self.total == 0 {
            return (self.domain_min / 2).wrapping_add(self.domain_max / 2);
        }
        let range = self.domain_max - self.domain_min;
        let bin_width = range / HIST_BINS as u32;
        let mut weighted_sum: u64 = 0;
        for i in 0..HIST_BINS {
            let center =
                self.domain_min as u64 + (bin_width as u64 * i as u64) + (bin_width as u64 / 2);
            weighted_sum += center * self.bins[i] as u64;
        }
        (weighted_sum / self.total as u64) as Weight
    }

    /// Entropy of the distribution in 16.16 fixed-point bits.
    ///
    /// Higher entropy means more uncertainty (wider spread).
    /// Zero entropy means deterministic (single bin).
    pub fn entropy(&self) -> Weight {
        if self.total == 0 {
            return 0;
        }
        // H = -sum(p_i * log2(p_i)), approximated in integer math.
        // We use H ≈ log2(total) - (1/total) * sum(count_i * log2(count_i))
        // with integer log2 approximation.
        let log_total = ilog2_fp(self.total);
        let mut sum_n_logn: u64 = 0;
        for i in 0..HIST_BINS {
            let c = self.bins[i];
            if c > 0 {
                sum_n_logn += c as u64 * ilog2_fp(c) as u64;
            }
        }
        let h = (log_total as u64 * self.total as u64).saturating_sub(sum_n_logn);
        // Normalize: h / total gives entropy in 16.16 bits
        (h / self.total as u64) as Weight
    }
}

/// Integer log2 in 16.16 fixed-point. Returns log2(n) * 65536.
fn ilog2_fp(n: u32) -> u32 {
    if n <= 1 {
        return 0;
    }
    // Integer part: position of highest set bit
    let int_part = 31 - n.leading_zeros(); // floor(log2(n))
    // Fractional part: linear interpolation between powers of 2
    let frac = if int_part < 31 {
        let lower = 1u32 << int_part;
        let upper = 1u32 << (int_part + 1);
        ((n - lower) as u64 * 65536 / (upper - lower) as u64) as u32
    } else {
        0
    };
    (int_part << 16) | frac
}

// ────────────────────────────────────────────────────────────────────
// Twin sensor node
// ────────────────────────────────────────────────────────────────────

/// A digital twin sensor node: hardware metric with history, prediction,
/// state distribution, and prediction error tracking.
pub struct TwinSensor {
    /// Graph node ID of this sensor in the arena.
    node_id: NodeId,
    /// What kind of sensor this is.
    kind: SensorKind,
    /// Active flag.
    active: bool,
    /// Ring buffer of observations.
    history: [Observation; SENSOR_HISTORY],
    /// Write head in the ring buffer.
    head: usize,
    /// Number of valid observations (saturates at SENSOR_HISTORY).
    count: usize,
    /// Current EMA-smoothed value (16.16).
    ema: Weight,
    /// State distribution (future state probability model).
    distribution: StateDistribution,
    /// Predicted values for the next PREDICTION_HORIZON steps.
    predictions: [Weight; PREDICTION_HORIZON],
    /// Prediction confidence (0–65536 = 0–100% in 16.16).
    prediction_confidence: Weight,
    /// Fiedler drift accumulator for this sensor's subgraph.
    drift_accumulator: i32,
    /// Consecutive drift steps above threshold.
    drift_steps: u32,
    /// Whether a predictive alarm is active.
    alarm: bool,
    /// Prediction error EMA: |actual − predicted[0]| smoothed (16.16).
    /// Tracks how wrong the predictor is in practice.
    prediction_error: Weight,
    /// Whether this sensor has a valid 1-step prediction to compare against.
    has_pending_prediction: bool,
    /// The 1-step-ahead prediction from the last cycle, to compare against
    /// the next actual observation for error tracking.
    pending_prediction: Weight,
}

impl TwinSensor {
    pub const fn empty() -> Self {
        Self {
            node_id: 0,
            kind: SensorKind::CpuTemperature,
            active: false,
            history: [Observation::EMPTY; SENSOR_HISTORY],
            head: 0,
            count: 0,
            ema: 0,
            distribution: StateDistribution::empty(0, WEIGHT_ONE),
            predictions: [0; PREDICTION_HORIZON],
            prediction_confidence: 0,
            drift_accumulator: 0,
            drift_steps: 0,
            alarm: false,
            prediction_error: 0,
            has_pending_prediction: false,
            pending_prediction: 0,
        }
    }

    // ── Read-only accessors ──────────────────────────────────────

    /// Returns the sensor kind.
    pub fn kind(&self) -> SensorKind {
        self.kind
    }
    /// Returns whether this sensor is active.
    pub fn is_active(&self) -> bool {
        self.active
    }
    /// Returns the number of observations recorded.
    pub fn observation_count(&self) -> usize {
        self.count
    }
    /// Returns the current EMA-smoothed value.
    pub fn ema(&self) -> Weight {
        self.ema
    }
    /// Returns the predictions array (read-only).
    pub fn predictions(&self) -> &[Weight; PREDICTION_HORIZON] {
        &self.predictions
    }
    /// Returns prediction confidence (0–WEIGHT_ONE).
    pub fn confidence(&self) -> Weight {
        self.prediction_confidence
    }
    /// Returns consecutive drift steps.
    pub fn drift_steps(&self) -> u32 {
        self.drift_steps
    }
    /// Returns whether an alarm is active.
    pub fn has_alarm(&self) -> bool {
        self.alarm
    }
    /// Returns the prediction error EMA.
    pub fn prediction_error(&self) -> Weight {
        self.prediction_error
    }
    /// Returns the graph node ID.
    pub fn node_id(&self) -> NodeId {
        self.node_id
    }

    // ── Mutators (package-private, called by TwinState) ──────────

    /// Deactivate this sensor. Clears the active flag.
    fn deactivate(&mut self) {
        self.active = false;
    }

    /// Reset all state (history, predictions, error tracking).
    fn reset(&mut self) {
        self.history = [Observation::EMPTY; SENSOR_HISTORY];
        self.head = 0;
        self.count = 0;
        self.ema = 0;
        self.predictions = [0; PREDICTION_HORIZON];
        self.prediction_confidence = 0;
        self.drift_accumulator = 0;
        self.drift_steps = 0;
        self.alarm = false;
        self.prediction_error = 0;
        self.has_pending_prediction = false;
        self.pending_prediction = 0;
        self.distribution = StateDistribution::empty(
            self.distribution.domain_min(),
            self.distribution.domain_max(),
        );
    }

    /// Ingest a new observation.
    ///
    /// Updates ring buffer, EMA, distribution, prediction error tracking,
    /// and runs linear prediction.
    pub fn observe(&mut self, timestamp: Timestamp, value: Weight) {
        // ── Prediction error tracking ────────────────────────────
        // Compare this actual observation against what we predicted last cycle.
        if self.has_pending_prediction {
            let error = value.abs_diff(self.pending_prediction);
            // EMA of absolute error
            let alpha = ERROR_ALPHA as u64;
            let one_minus = (WEIGHT_ONE as u64).saturating_sub(alpha);
            self.prediction_error =
                ((alpha * error as u64 + one_minus * self.prediction_error as u64) >> 16) as Weight;
        }

        // ── Update EMA ───────────────────────────────────────────
        // ema = alpha * value + (1 - alpha) * ema
        // Use u64 intermediate to avoid precision loss from premature truncation.
        let alpha = EMA_ALPHA as u64;
        let one_minus_alpha = (WEIGHT_ONE as u64).saturating_sub(alpha);
        let new_ema =
            (alpha * value as u64 + one_minus_alpha * self.ema as u64 + (1u64 << 15)) >> 16;
        self.ema = new_ema.min(u32::MAX as u64) as Weight;

        let obs = Observation {
            timestamp,
            value,
            smoothed: self.ema,
            _pad: 0,
        };

        self.history[self.head] = obs;
        self.head = (self.head + 1) % SENSOR_HISTORY;
        if self.count < SENSOR_HISTORY {
            self.count += 1;
        }

        // Update state distribution
        self.distribution.record(value);

        // Run linear prediction if we have enough history
        if self.count >= 8 {
            self.predict_linear();
            // Save the 1-step prediction for error tracking on next observation.
            self.has_pending_prediction = true;
            self.pending_prediction = self.predictions[0];
        }
    }

    /// Simple linear extrapolation from the last N smoothed values.
    ///
    /// Uses least-squares slope estimation on the EMA series, then
    /// projects forward PREDICTION_HORIZON steps. Confidence is reduced
    /// by the prediction error EMA to detect model breakdown.
    fn predict_linear(&mut self) {
        let n = self.count.min(32); // Use last 32 points for trend
        if n < 4 {
            return;
        }

        // Collect the last N smoothed values (newest first)
        let mut vals = [0i64; 32];
        for (i, val) in vals.iter_mut().take(n).enumerate() {
            let idx = (self.head + SENSOR_HISTORY - 1 - i) % SENSOR_HISTORY;
            *val = self.history[idx].smoothed as i64;
        }

        // Reverse so vals[0] = oldest
        vals[..n].reverse();

        // Least-squares slope: slope = (n * sum(i*y) - sum(i)*sum(y)) / (n * sum(i^2) - sum(i)^2)
        let nn = n as i64;
        let mut sum_i: i64 = 0;
        let mut sum_y: i64 = 0;
        let mut sum_iy: i64 = 0;
        let mut sum_i2: i64 = 0;
        for (i, val) in vals.iter().take(n).enumerate() {
            let ii = i as i64;
            sum_i += ii;
            sum_y += *val;
            sum_iy += ii * *val;
            sum_i2 += ii * ii;
        }

        let denom = nn * sum_i2 - sum_i * sum_i;
        if denom == 0 {
            // Flat line — predict current EMA for all horizons
            for h in 0..PREDICTION_HORIZON {
                self.predictions[h] = self.ema;
            }
            self.prediction_confidence = WEIGHT_ONE; // 100% confident in flat
            return;
        }

        let slope_num = nn * sum_iy - sum_i * sum_y;
        let intercept_num = sum_y * sum_i2 - sum_i * sum_iy;

        // Extrapolate: y(n + h) = intercept + slope * (n + h)
        // Use checked arithmetic to detect overflow.
        for h in 0..PREDICTION_HORIZON {
            let t = (n as i64) + (h as i64);
            // Compute (intercept_num + slope_num * t) / denom with overflow check
            let slope_term =
                slope_num
                    .checked_mul(t)
                    .unwrap_or(if slope_num > 0 { i64::MAX } else { i64::MIN });
            let numerator = intercept_num.saturating_add(slope_term);
            let pred = numerator / denom;
            // Clamp to valid Weight range [0, u32::MAX]
            self.predictions[h] = pred.max(0).min(u32::MAX as i64) as Weight;
        }

        // Confidence: inverse of normalized prediction variance
        // Higher variance → lower confidence
        let mean_pred: i64 =
            self.predictions.iter().map(|&p| p as i64).sum::<i64>() / PREDICTION_HORIZON as i64;
        let variance: i64 = self
            .predictions
            .iter()
            .map(|&p| {
                let diff = p as i64 - mean_pred;
                diff * diff
            })
            .sum::<i64>()
            / PREDICTION_HORIZON as i64;

        // Confidence = 1.0 / (1.0 + variance / last_val^2) in 16.16
        let last_val = vals[n - 1];
        let scale = if last_val > 0 {
            ((last_val * last_val) >> 16) as u64
        } else {
            WEIGHT_ONE as u64
        };
        let variance_conf = if scale == 0 {
            0
        } else {
            let ratio = (variance as u64 * WEIGHT_ONE as u64)
                .checked_div(scale)
                .unwrap_or(0);
            let denom_conf = WEIGHT_ONE as u64 + ratio;
            (WEIGHT_ONE as u64 * WEIGHT_ONE as u64)
                .checked_div(denom_conf)
                .unwrap_or(0) as Weight
        };

        // Apply prediction error penalty: reduce confidence proportional to
        // how wrong the model has been.  error_ratio = error_ema / domain_range.
        // At error_ratio = 0.5, confidence is halved.
        let domain_range = self
            .distribution
            .domain_max()
            .saturating_sub(self.distribution.domain_min())
            .max(1) as u64;
        let error_ratio = (self.prediction_error as u64 * WEIGHT_ONE as u64) / domain_range;
        // penalty = 1 / (1 + error_ratio)
        let error_denom = WEIGHT_ONE as u64 + error_ratio;
        let error_penalty = (WEIGHT_ONE as u64 * WEIGHT_ONE as u64)
            .checked_div(error_denom)
            .unwrap_or(0) as Weight;

        // Final confidence = variance_conf * error_penalty / WEIGHT_ONE
        self.prediction_confidence =
            ((variance_conf as u64 * error_penalty as u64) >> 16) as Weight;
    }

    /// Update Fiedler drift tracking.
    ///
    /// Called by the spectral subsystem when a new snapshot is taken.
    /// `delta_fiedler` is λ₂(t) - λ₂(t-1) in signed 16.16.
    pub fn update_drift(&mut self, delta_fiedler: i32) {
        self.drift_accumulator = self.drift_accumulator.saturating_add(delta_fiedler);

        if delta_fiedler < FIEDLER_DRIFT_THRESHOLD {
            self.drift_steps = self.drift_steps.saturating_add(1);
        } else {
            // Exponential decay: halve drift_steps on recovery.
            // This prevents brief spikes from permanently elevating drift.
            self.drift_steps /= 2;
        }

        self.alarm = self.drift_steps >= DRIFT_SUSTAIN_STEPS;
    }
}

// ────────────────────────────────────────────────────────────────────
// Entanglement link — causal propagation between twin sensors
// ────────────────────────────────────────────────────────────────────

/// A causal dependency between two twin sensors.
///
/// When the source sensor's state changes, the destination sensor's
/// predicted distribution is shifted proportionally — this is the
/// "entanglement" in the quantum-inspired model.
///
/// Discovered by Granger causality or transfer entropy analysis
/// on the sensor time series.
///
/// 24 bytes, repr(C), Copy.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct CausalLink {
    source: u8,
    dest: u8,
    lag: u8,
    active: bool,
    coupling: Weight,
    direction: Weight,
    transfer_entropy: Weight,
    granger_sig: Weight,
}

impl CausalLink {
    pub const EMPTY: Self = Self {
        source: 0,
        dest: 0,
        lag: 0,
        active: false,
        coupling: 0,
        direction: 0,
        transfer_entropy: 0,
        granger_sig: 0,
    };

    pub const fn new(source: u8, dest: u8, coupling: Weight, lag: u8) -> Self {
        Self {
            source,
            dest,
            lag,
            active: true,
            coupling,
            direction: WEIGHT_ONE,
            transfer_entropy: 0,
            granger_sig: 0,
        }
    }

    pub fn source(&self) -> u8 {
        self.source
    }
    pub fn dest(&self) -> u8 {
        self.dest
    }
    pub fn lag(&self) -> u8 {
        self.lag
    }
    pub fn is_active(&self) -> bool {
        self.active
    }
    pub fn coupling(&self) -> Weight {
        self.coupling
    }
    pub fn direction(&self) -> Weight {
        self.direction
    }
    pub fn transfer_entropy_val(&self) -> Weight {
        self.transfer_entropy
    }
    pub fn granger_sig(&self) -> Weight {
        self.granger_sig
    }

    pub fn set_coupling(&mut self, c: Weight) {
        self.coupling = c;
    }
    pub fn set_granger_sig(&mut self, g: Weight) {
        self.granger_sig = g;
    }
    pub fn set_transfer_entropy(&mut self, te: Weight) {
        self.transfer_entropy = te;
    }
    pub fn deactivate(&mut self) {
        self.active = false;
    }
}

pub const MAX_CAUSAL_LINKS: usize = 64;

// ────────────────────────────────────────────────────────────────────
// Prediction alarm
// ────────────────────────────────────────────────────────────────────

/// A predictive alarm raised by the twin engine.
///
/// 32 bytes, repr(C), Copy.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct PredictionAlarm {
    /// Timestamp when the alarm was raised.
    timestamp: Timestamp,
    /// Sensor that triggered the alarm.
    sensor_idx: u8,
    /// Alarm severity.
    severity: AlarmSeverity,
    /// Padding.
    _pad: [u8; 2],
    /// Predicted value that triggered the alarm (16.16).
    predicted_value: Weight,
    /// Current value at alarm time (16.16).
    current_value: Weight,
    /// Steps until predicted threshold breach.
    steps_to_breach: u8,
    /// Root cause sensor index (from causal tracing), or 0xFF if unknown.
    root_cause: u8,
    /// Padding.
    _pad2: [u8; 2],
}

impl PredictionAlarm {
    pub const EMPTY: Self = Self {
        timestamp: 0,
        sensor_idx: 0,
        severity: AlarmSeverity::Info,
        _pad: [0; 2],
        predicted_value: 0,
        current_value: 0,
        steps_to_breach: 0,
        root_cause: 0xFF,
        _pad2: [0; 2],
    };

    pub fn timestamp(&self) -> Timestamp {
        self.timestamp
    }
    pub fn sensor_idx(&self) -> u8 {
        self.sensor_idx
    }
    pub fn severity(&self) -> AlarmSeverity {
        self.severity
    }
    pub fn predicted_value(&self) -> Weight {
        self.predicted_value
    }
    pub fn current_value(&self) -> Weight {
        self.current_value
    }
    pub fn steps_to_breach(&self) -> u8 {
        self.steps_to_breach
    }
    pub fn root_cause(&self) -> u8 {
        self.root_cause
    }
}

/// Alarm severity levels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum AlarmSeverity {
    /// Informational: prediction diverges but within safe bounds.
    Info = 0,
    /// Warning: predicted threshold breach within PREDICTION_HORIZON.
    Warning = 1,
    /// Critical: predicted breach within 2 steps, immediate action needed.
    Critical = 2,
}

/// Maximum pending alarms.
pub const MAX_ALARMS: usize = 16;

// ────────────────────────────────────────────────────────────────────
// Twin state — the complete digital twin
// ────────────────────────────────────────────────────────────────────

/// The digital twin engine state.
///
/// Holds all sensor nodes, entanglement links, prediction state, and
/// alarm queue. Statically allocated, no heap.
pub struct TwinState {
    /// Twin sensor nodes.
    sensors: [TwinSensor; MAX_TWIN_SENSORS],
    /// Number of active sensors.
    sensor_count: usize,
    /// Causal links between sensors.
    causal_links: [CausalLink; MAX_CAUSAL_LINKS],
    /// Number of active causal links.
    link_count: usize,
    /// Pending prediction alarms.
    alarms: [PredictionAlarm; MAX_ALARMS],
    /// Number of pending alarms.
    alarm_count: usize,
    /// Global twin generation counter (incremented on each observation cycle).
    generation: u64,
    /// Overall system coherence score (16.16).
    /// High coherence = stable system, low = impending state change.
    coherence: Weight,
    /// Global entropy of the twin (sum of sensor entropies, 16.16).
    /// Low entropy = predictable, high = uncertain.
    total_entropy: Weight,
}

impl TwinState {
    pub const fn new() -> Self {
        Self {
            sensors: [const { TwinSensor::empty() }; MAX_TWIN_SENSORS],
            sensor_count: 0,
            causal_links: [CausalLink::EMPTY; MAX_CAUSAL_LINKS],
            link_count: 0,
            alarms: [PredictionAlarm::EMPTY; MAX_ALARMS],
            alarm_count: 0,
            generation: 0,
            coherence: WEIGHT_ONE,
            total_entropy: 0,
        }
    }

    // ── Read-only accessors ──────────────────────────────────────
    pub fn sensor_count(&self) -> usize {
        self.sensor_count
    }
    pub fn link_count(&self) -> usize {
        self.link_count
    }
    pub fn alarm_count(&self) -> usize {
        self.alarm_count
    }
    pub fn generation(&self) -> u64 {
        self.generation
    }
    pub fn coherence(&self) -> Weight {
        self.coherence
    }
    pub fn total_entropy(&self) -> Weight {
        self.total_entropy
    }
    /// Read-only access to a sensor by index.
    pub fn sensor(&self, idx: usize) -> Option<&TwinSensor> {
        if idx < self.sensor_count {
            Some(&self.sensors[idx])
        } else {
            None
        }
    }

    /// Register a new twin sensor. Returns the sensor index or None if full.
    pub fn register_sensor(
        &mut self,
        node_id: NodeId,
        kind: SensorKind,
        domain_min: Weight,
        domain_max: Weight,
    ) -> Option<usize> {
        if self.sensor_count >= MAX_TWIN_SENSORS {
            return None;
        }
        let idx = self.sensor_count;
        self.sensors[idx].node_id = node_id;
        self.sensors[idx].kind = kind;
        self.sensors[idx].active = true;
        self.sensors[idx].distribution = StateDistribution::empty(domain_min, domain_max);
        self.sensor_count += 1;
        Some(idx)
    }

    /// Register a causal link between two sensors.
    pub fn register_link(
        &mut self,
        source: u8,
        dest: u8,
        coupling: Weight,
        lag: u8,
    ) -> Option<usize> {
        if self.link_count >= MAX_CAUSAL_LINKS {
            return None;
        }
        let idx = self.link_count;
        self.causal_links[idx] = CausalLink::new(source, dest, coupling, lag);
        self.link_count += 1;
        Some(idx)
    }

    /// Ingest a telemetry observation for a sensor.
    ///
    /// This is the primary ingestion path. After recording the observation,
    /// it propagates through causal links to shift coupled sensors'
    /// predicted values.
    pub fn observe(&mut self, sensor_idx: usize, timestamp: Timestamp, value: Weight) {
        if sensor_idx >= self.sensor_count || !self.sensors[sensor_idx].active {
            return;
        }

        self.sensors[sensor_idx].observe(timestamp, value);

        // Propagate through causal links
        self.propagate_causal(sensor_idx, value);

        self.generation = self.generation.wrapping_add(1);
    }

    /// Propagate a state change through causal links.
    ///
    /// When sensor `source_idx` changes, every linked destination sensor
    /// has its predictions shifted proportional to coupling * deviation.
    /// The deviation is how much the source deviated from its OWN baseline
    /// (source EMA), not the destination's.
    fn propagate_causal(&mut self, source_idx: usize, new_value: Weight) {
        // Capture source EMA before mutation (already updated by observe()).
        let source_ema = self.sensors[source_idx].ema();

        for i in 0..self.link_count {
            let link = self.causal_links[i];
            if !link.is_active() || link.source() as usize != source_idx {
                continue;
            }
            let dest_idx = link.dest() as usize;
            if dest_idx >= self.sensor_count {
                continue;
            }

            // Delta = how much source deviated from its own baseline.
            // Allow both positive and negative deviations (no upward bias).
            let delta = new_value.abs_diff(source_ema);

            // Shift = coupling * delta (16.16 multiply)
            let shift = ((link.coupling() as u64 * delta as u64) >> 16) as Weight;

            if shift > 0 {
                // Direction: if source increased, shift predictions up;
                // if decreased, shift down.
                let upward = new_value >= source_ema;
                for h in 0..PREDICTION_HORIZON {
                    if upward {
                        self.sensors[dest_idx].predictions[h] =
                            self.sensors[dest_idx].predictions[h].saturating_add(shift);
                    } else {
                        self.sensors[dest_idx].predictions[h] =
                            self.sensors[dest_idx].predictions[h].saturating_sub(shift);
                    }
                }
            }
        }
    }

    /// Run a full prediction cycle across all sensors.
    ///
    /// Updates coherence score and checks for threshold breaches.
    pub fn predict_cycle(&mut self, timestamp: Timestamp) {
        // Update total entropy
        let mut total_ent: u64 = 0;
        let mut active_count = 0u64;
        for i in 0..self.sensor_count {
            if self.sensors[i].is_active() {
                total_ent += self.sensors[i].distribution.entropy() as u64;
                active_count += 1;
            }
        }
        self.total_entropy = total_ent.checked_div(active_count).unwrap_or(0) as Weight;

        // Check for prediction threshold breaches and raise alarms
        self.check_thresholds(timestamp);

        // Update coherence: inverse of alarm severity sum
        let alarm_severity_sum: u32 = self.alarms[..self.alarm_count]
            .iter()
            .map(|a| match a.severity() {
                AlarmSeverity::Info => 1u32,
                AlarmSeverity::Warning => 3,
                AlarmSeverity::Critical => 10,
            })
            .sum();

        self.coherence = if alarm_severity_sum == 0 {
            WEIGHT_ONE
        } else {
            // coherence = 1.0 / (1.0 + severity_sum * 0.1)
            let denom = WEIGHT_ONE as u64 + (alarm_severity_sum as u64 * 6554);
            ((WEIGHT_ONE as u64 * WEIGHT_ONE as u64) / denom) as Weight
        };
    }

    /// Check sensor predictions against thresholds and raise alarms.
    ///
    /// Alarms expire after 16 predict_cycle calls. New alarms are only
    /// raised if a matching alarm doesn't already exist, preventing
    /// the permanent-alarm SMELL #1 that kills coherence.
    fn check_thresholds(&mut self, timestamp: Timestamp) {
        // Expire old alarms: remove any that are older than 16 generations.
        let cur_gen = self.generation;
        let mut write = 0;
        for read in 0..self.alarm_count {
            if cur_gen.wrapping_sub(self.alarms[read].timestamp()) < 16 * PREDICT_EVERY {
                self.alarms[write] = self.alarms[read];
                write += 1;
            }
        }
        self.alarm_count = write;

        for i in 0..self.sensor_count {
            if !self.sensors[i].is_active() {
                continue;
            }

            let threshold = self.sensors[i].distribution.domain_max();

            // Check if any prediction exceeds 90% of domain max
            let warning_threshold = threshold - (threshold / 10);
            // Critical: exceeds 95%
            let critical_threshold = threshold - (threshold / 20);

            // Skip if we already have an alarm for this sensor.
            let already_alarmed = self.alarms[..self.alarm_count]
                .iter()
                .any(|a| a.sensor_idx() as usize == i);
            if already_alarmed {
                continue;
            }

            for h in 0..PREDICTION_HORIZON {
                let pred = self.sensors[i].predictions()[h];
                if pred >= critical_threshold && self.alarm_count < MAX_ALARMS {
                    let root = self.trace_root_cause(i);
                    self.alarms[self.alarm_count] = PredictionAlarm {
                        timestamp,
                        sensor_idx: i as u8,
                        severity: AlarmSeverity::Critical,
                        _pad: [0; 2],
                        predicted_value: pred,
                        current_value: self.sensors[i].ema(),
                        steps_to_breach: h as u8,
                        root_cause: root,
                        _pad2: [0; 2],
                    };
                    self.alarm_count += 1;
                    break;
                } else if pred >= warning_threshold && self.alarm_count < MAX_ALARMS {
                    let root = self.trace_root_cause(i);
                    self.alarms[self.alarm_count] = PredictionAlarm {
                        timestamp,
                        sensor_idx: i as u8,
                        severity: AlarmSeverity::Warning,
                        _pad: [0; 2],
                        predicted_value: pred,
                        current_value: self.sensors[i].ema(),
                        steps_to_breach: h as u8,
                        root_cause: root,
                        _pad2: [0; 2],
                    };
                    self.alarm_count += 1;
                    break;
                }
            }

            // Also check Fiedler drift alarm
            if self.sensors[i].has_alarm() && self.alarm_count < MAX_ALARMS {
                self.alarms[self.alarm_count] = PredictionAlarm {
                    timestamp,
                    sensor_idx: i as u8,
                    severity: AlarmSeverity::Warning,
                    _pad: [0; 2],
                    predicted_value: self.sensors[i].predictions()[0],
                    current_value: self.sensors[i].ema(),
                    steps_to_breach: 0,
                    root_cause: 0xFF,
                    _pad2: [0; 2],
                };
                self.alarm_count += 1;
            }
        }
    }

    /// Trace the root cause of a predicted anomaly by following
    /// entanglement links backwards.
    ///
    /// Returns the sensor index of the most likely root cause,
    /// or 0xFF if no causal path is found.
    fn trace_root_cause(&self, alarm_sensor: usize) -> u8 {
        let mut best_cause: u8 = 0xFF;
        let mut best_coupling: Weight = 0;

        for i in 0..self.link_count {
            let link = &self.causal_links[i];
            if !link.is_active() || link.dest() as usize != alarm_sensor {
                continue;
            }
            let src = link.source() as usize;
            if src >= self.sensor_count {
                continue;
            }

            // Prefer sensors that are themselves alarming or drifting
            let src_severity = if self.sensors[src].has_alarm() {
                link.coupling().saturating_add(WEIGHT_ONE)
            } else {
                link.coupling()
            };

            if src_severity > best_coupling {
                best_coupling = src_severity;
                best_cause = link.source();
            }
        }

        best_cause
    }

    /// Query the twin for a specific sensor's prediction.
    ///
    /// Returns (predicted_value, confidence) for the requested horizon step.
    pub fn query_prediction(&self, sensor_idx: usize, horizon: usize) -> (Weight, Weight) {
        if sensor_idx >= self.sensor_count || horizon >= PREDICTION_HORIZON {
            return (0, 0);
        }
        let sensor = &self.sensors[sensor_idx];
        (sensor.predictions()[horizon], sensor.confidence())
    }

    /// Query the expected value from the state distribution.
    pub fn query_expected(&self, sensor_idx: usize) -> Weight {
        if sensor_idx >= self.sensor_count {
            return 0;
        }
        self.sensors[sensor_idx].distribution.collapse()
    }

    /// Dump twin state to serial for diagnostics.
    pub fn dump(&self) {
        use crate::arch::serial;

        serial::write_line(b"[twin] === Twin State Dump ===");
        serial::write_bytes(b"[twin] sensors=");
        serial::write_u64_dec_inline(self.sensor_count as u64);
        serial::write_bytes(b"  links=");
        serial::write_u64_dec_inline(self.link_count as u64);
        serial::write_bytes(b"  alarms=");
        serial::write_u64_dec_inline(self.alarm_count as u64);
        serial::write_bytes(b"  gen=");
        serial::write_u64_dec_inline(self.generation);
        serial::write_bytes(b"  coherence=");
        serial::write_u64_dec_inline(self.coherence as u64);
        serial::write_bytes(b"  entropy=");
        serial::write_u64_dec(self.total_entropy as u64);

        for i in 0..self.sensor_count {
            let s = &self.sensors[i];
            if !s.is_active() {
                continue;
            }
            serial::write_bytes(b"  S");
            serial::write_u64_dec_inline(i as u64);
            serial::write_bytes(b" kind=");
            serial::write_u64_dec_inline(s.kind() as u64);
            serial::write_bytes(b" obs=");
            serial::write_u64_dec_inline(s.observation_count() as u64);
            serial::write_bytes(b" ema=");
            serial::write_u64_dec_inline(s.ema() as u64);
            serial::write_bytes(b" pred[0]=");
            serial::write_u64_dec_inline(s.predictions()[0] as u64);
            serial::write_bytes(b" conf=");
            serial::write_u64_dec_inline(s.confidence() as u64);
            serial::write_bytes(b" drift=");
            serial::write_u64_dec_inline(s.drift_steps() as u64);
            serial::write_bytes(b" err=");
            serial::write_u64_dec_inline(s.prediction_error() as u64);
            serial::write_bytes(b" alarm=");
            if s.has_alarm() {
                serial::write_line(b"YES");
            } else {
                serial::write_line(b"no");
            }
        }
        serial::write_line(b"[twin] === End Twin Dump ===");
    }
}

// ────────────────────────────────────────────────────────────────────
// Global twin instance
// ────────────────────────────────────────────────────────────────────

use spin::Mutex;

/// The global digital twin state, protected by a spin lock.
pub static TWIN: Mutex<TwinState> = Mutex::new(TwinState::new());

// ────────────────────────────────────────────────────────────────────
// Well-known sensor indices — set by twin_init(), read by ingestion
// ────────────────────────────────────────────────────────────────────

use core::sync::atomic::{AtomicU8, AtomicU64, Ordering};

/// Sentinel: sensor not registered.
const SENSOR_NONE: u8 = 0xFF;

/// Well-known sensor slot indices, set once at init.
static SENSOR_CPU_TEMP: AtomicU8 = AtomicU8::new(SENSOR_NONE);
static SENSOR_CPU_UTIL: AtomicU8 = AtomicU8::new(SENSOR_NONE);
static SENSOR_MEM_UTIL: AtomicU8 = AtomicU8::new(SENSOR_NONE);
static SENSOR_IRQ_RATE: AtomicU8 = AtomicU8::new(SENSOR_NONE);
static SENSOR_IPC_TP: AtomicU8 = AtomicU8::new(SENSOR_NONE);
static IPC_TELEMETRY_ENABLED: AtomicU8 = AtomicU8::new(0);
static SENSOR_CACHE_HR: AtomicU8 = AtomicU8::new(SENSOR_NONE);
static SENSOR_DISPLAY_FPS: AtomicU8 = AtomicU8::new(SENSOR_NONE);
static SENSOR_DISPLAY_BW: AtomicU8 = AtomicU8::new(SENSOR_NONE);
static SENSOR_DISPLAY_COVERAGE: AtomicU8 = AtomicU8::new(SENSOR_NONE);

/// Bind a sensor kind to its well-known index after registration.
pub fn bind_sensor(kind: SensorKind, idx: usize) {
    let slot = match kind {
        SensorKind::CpuTemperature => &SENSOR_CPU_TEMP,
        SensorKind::CpuUtilization => &SENSOR_CPU_UTIL,
        SensorKind::MemoryUtilization => &SENSOR_MEM_UTIL,
        SensorKind::InterruptRate => &SENSOR_IRQ_RATE,
        SensorKind::IpcThroughput => &SENSOR_IPC_TP,
        SensorKind::CacheHitRate => &SENSOR_CACHE_HR,
        SensorKind::DisplayRefreshRate => &SENSOR_DISPLAY_FPS,
        SensorKind::DisplayBandwidth => &SENSOR_DISPLAY_BW,
        SensorKind::DisplayCoverage => &SENSOR_DISPLAY_COVERAGE,
        _ => return,
    };
    slot.store(idx as u8, Ordering::Release);
}

/// Whether the twin is online (all sensors bound, ready for ingestion).
static TWIN_ONLINE: AtomicU8 = AtomicU8::new(0);

/// Mark the twin engine as online. Called after twin_init() completes.
pub fn set_online() {
    TWIN_ONLINE.store(1, Ordering::Release);
}

pub fn set_ipc_telemetry_enabled(enabled: bool) {
    IPC_TELEMETRY_ENABLED.store(enabled as u8, Ordering::Release);
}

#[inline]
fn is_online() -> bool {
    TWIN_ONLINE.load(Ordering::Acquire) != 0
}

// ────────────────────────────────────────────────────────────────────
// Ingestion counters (atomics — updated in ISR, drained by observe)
// ────────────────────────────────────────────────────────────────────

/// Total PIT ticks since last IRQ-rate observation was pushed.
static IRQ_TICKS_ACCUM: AtomicU64 = AtomicU64::new(0);
static LAST_DISPLAY_TICK: AtomicU64 = AtomicU64::new(0);

/// Observation generation — bumped on every predict_cycle call.
/// Used to amortize prediction cost: only run predict_cycle every
/// PREDICT_EVERY observations across all sensors combined.
static OBS_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Samples dropped because the twin lock was contended.
/// Monotonically increasing. Useful for observability / diagnostics.
static DROPPED_SAMPLES: AtomicU64 = AtomicU64::new(0);

/// Returns the number of samples dropped due to twin lock contention.
pub fn dropped_sample_count() -> u64 {
    DROPPED_SAMPLES.load(Ordering::Relaxed)
}

/// Run predict_cycle every this many observations.
const PREDICT_EVERY: u64 = 64;

// ────────────────────────────────────────────────────────────────────
// Event-driven ingestion API — called from subsystem hot paths
// ────────────────────────────────────────────────────────────────────

/// Called from the PIT timer ISR on every tick.
///
/// Ultra-cheap fast path: just increments an atomic counter.
/// Every 100 ticks (100 ms at 1 kHz), pushes an IRQ-rate observation
/// into the twin (100 ms window → multiply by 10 for per-second rate).
///
/// The actual twin lock is taken at most 10 times per second.
#[inline]
pub fn ingest_irq_tick(tick: u64) {
    if !is_online() {
        return;
    }

    // BUG #1 fix: compare_exchange loop avoids TOCTOU race.
    // Two ISRs can no longer both see acc>=100 and both reset to 0.
    let acc = IRQ_TICKS_ACCUM.fetch_add(1, Ordering::Relaxed) + 1;
    if acc < 100 {
        return;
    }
    // Atomically drain: if another ISR beat us, CAS fails and we skip.
    match IRQ_TICKS_ACCUM.compare_exchange(acc, 0, Ordering::AcqRel, Ordering::Relaxed) {
        Ok(_) => {}
        Err(_) => return, // another path drained it; skip this cycle
    };

    let idx = SENSOR_IRQ_RATE.load(Ordering::Acquire);
    if idx == SENSOR_NONE {
        return;
    }

    // 100 ticks in 100 ms → rate = acc * 10 per second, in 16.16
    // BUG #4 fix: saturating_mul + clamp to u32::MAX prevents truncation.
    let rate_fp = acc
        .saturating_mul(10)
        .saturating_mul(WEIGHT_ONE as u64)
        .min(u32::MAX as u64);

    if let Some(mut twin) = TWIN.try_lock() {
        twin.observe(idx as usize, tick, rate_fp as Weight);
        maybe_predict(&mut twin, tick);
    } else {
        DROPPED_SAMPLES.fetch_add(1, Ordering::Relaxed);
    }
    // If lock contended (another core / re-entrant), skip — next tick will catch up.
}

/// Called from the scheduler after a context switch completes.
///
/// Computes CPU utilization as: fraction of the last scheduler time slice that
/// was actually used before preemption. `quantum_used` = slice_total - remaining.
/// Utilization = used / total in 16.16.
#[inline]
pub fn ingest_context_switch(tick: u64, quantum_used: u64, quantum_total: u64) {
    if !is_online() {
        return;
    }

    let idx = SENSOR_CPU_UTIL.load(Ordering::Acquire);
    if idx == SENSOR_NONE {
        return;
    }

    // util = used / total in 16.16
    let util_fp = quantum_used
        .checked_mul(WEIGHT_ONE as u64)
        .and_then(|scaled| scaled.checked_div(quantum_total))
        .unwrap_or(0) as Weight;

    if let Some(mut twin) = TWIN.try_lock() {
        twin.observe(idx as usize, tick, util_fp);

        // Synthetic CPU temperature model: temp = 35 + 65 * util
        // (35°C idle, 100°C at 100% — reasonable for QEMU fiction)
        let temp_idx = SENSOR_CPU_TEMP.load(Ordering::Acquire);
        if temp_idx != SENSOR_NONE {
            let base = 35u64 * WEIGHT_ONE as u64;
            let delta = (65u64 * util_fp as u64 * WEIGHT_ONE as u64) >> 16;
            // BUG #3 fix: clamp to u32::MAX before cast.
            let temp_fp = (base + delta).min(u32::MAX as u64) as Weight;
            twin.observe(temp_idx as usize, tick, temp_fp);
        }

        // Synthetic cache hit rate model: cache = 0.98 - 0.3 * util
        // (high util degrades cache locality)
        let cache_idx = SENSOR_CACHE_HR.load(Ordering::Acquire);
        if cache_idx != SENSOR_NONE {
            let base = (WEIGHT_ONE as u64 * 98) / 100; // 0.98
            let penalty = (30u64 * util_fp as u64) / 100; // 0.3 * util
            let cache_fp = base.saturating_sub(penalty) as Weight;
            twin.observe(cache_idx as usize, tick, cache_fp);
        }

        maybe_predict(&mut twin, tick);
    } else {
        DROPPED_SAMPLES.fetch_add(1, Ordering::Relaxed);
    }
}

/// Called from the frame allocator after alloc or dealloc.
///
/// Pushes memory utilization = allocated / (allocated + available).
#[inline]
pub fn ingest_frame_event(tick: u64, allocated: usize, total: usize) {
    if !is_online() {
        return;
    }

    let idx = SENSOR_MEM_UTIL.load(Ordering::Acquire);
    if idx == SENSOR_NONE {
        return;
    }

    let util_fp = (allocated as u64)
        .checked_mul(WEIGHT_ONE as u64)
        .and_then(|scaled| scaled.checked_div(total as u64))
        .unwrap_or(0) as Weight;

    if let Some(mut twin) = TWIN.try_lock() {
        twin.observe(idx as usize, tick, util_fp);
        maybe_predict(&mut twin, tick);
    } else {
        DROPPED_SAMPLES.fetch_add(1, Ordering::Relaxed);
    }
}

/// Called from IPC send path after a message is enqueued.
///
/// Pushes a "1 message" event. The twin's EMA naturally smooths this
/// into a messages-per-observation-window rate.
#[inline]
pub fn ingest_ipc_send(tick: u64) {
    if !is_online() || IPC_TELEMETRY_ENABLED.load(Ordering::Acquire) == 0 {
        return;
    }

    let idx = SENSOR_IPC_TP.load(Ordering::Acquire);
    if idx == SENSOR_NONE {
        return;
    }

    // Each call = 1 message event. Value = 1.0 in 16.16.
    // The EMA in the twin sensor will smooth this into a rate.
    if let Some(mut twin) = TWIN.try_lock() {
        twin.observe(idx as usize, tick, WEIGHT_ONE);
        maybe_predict(&mut twin, tick);
    } else {
        DROPPED_SAMPLES.fetch_add(1, Ordering::Relaxed);
    }
}

/// Called from the display driver after a scanout write or present.
///
/// This exposes three graph-facing display metrics:
/// - refresh cadence in presents/sec
/// - write bandwidth in MiB/sec
/// - dirty coverage as a fraction of the visible scanout
#[inline]
pub fn ingest_display_present(tick: u64, pixels: u64, bytes: u64, full_surface_pixels: u64) {
    if !is_online() {
        return;
    }

    let fps_idx = SENSOR_DISPLAY_FPS.load(Ordering::Acquire);
    let bw_idx = SENSOR_DISPLAY_BW.load(Ordering::Acquire);
    let coverage_idx = SENSOR_DISPLAY_COVERAGE.load(Ordering::Acquire);
    if fps_idx == SENSOR_NONE && bw_idx == SENSOR_NONE && coverage_idx == SENSOR_NONE {
        return;
    }

    let previous = LAST_DISPLAY_TICK.swap(tick, Ordering::AcqRel);
    let delta_ticks = tick.saturating_sub(previous);
    let fps_fp = if previous == 0 || delta_ticks == 0 {
        0
    } else {
        (1000u64.saturating_mul(WEIGHT_ONE as u64) / delta_ticks).min(u32::MAX as u64) as Weight
    };

    let bandwidth_fp = if previous == 0 || delta_ticks == 0 {
        0
    } else {
        let bytes_per_second = bytes.saturating_mul(1000).saturating_div(delta_ticks);
        (bytes_per_second.saturating_mul(WEIGHT_ONE as u64) / (1024 * 1024)).min(u32::MAX as u64)
            as Weight
    };

    let coverage_fp = pixels
        .min(full_surface_pixels)
        .saturating_mul(WEIGHT_ONE as u64)
        .checked_div(full_surface_pixels)
        .unwrap_or(0)
        .min(u32::MAX as u64) as Weight;

    if let Some(mut twin) = TWIN.try_lock() {
        if fps_idx != SENSOR_NONE {
            twin.observe(fps_idx as usize, tick, fps_fp);
        }
        if bw_idx != SENSOR_NONE {
            twin.observe(bw_idx as usize, tick, bandwidth_fp);
        }
        if coverage_idx != SENSOR_NONE {
            twin.observe(coverage_idx as usize, tick, coverage_fp);
        }
        maybe_predict(&mut twin, tick);
    } else {
        DROPPED_SAMPLES.fetch_add(1, Ordering::Relaxed);
    }
}

/// Amortized prediction: only run predict_cycle every PREDICT_EVERY
/// observations to keep ISR overhead minimal.
#[inline]
fn maybe_predict(twin: &mut TwinState, tick: u64) {
    let n = OBS_COUNTER.fetch_add(1, Ordering::Relaxed) + 1;
    if n.is_multiple_of(PREDICT_EVERY) {
        twin.predict_cycle(tick);
    }
}

// ────────────────────────────────────────────────────────────────────
// Predictive scheduling hint — queried by the scheduler
// ────────────────────────────────────────────────────────────────────

/// Scheduling hint produced by the digital twin for the scheduler.
///
/// This is the interface between the prediction engine and the dispatch
/// loop: the twin outputs a concrete recommendation that the scheduler
/// can act on without understanding the prediction internals.
#[derive(Debug, Clone, Copy)]
pub struct SchedHint {
    /// Recommended scheduler time slice in PIT ticks.
    /// `SCHED_TIME_SLICE_TICKS` (10) = normal, less = throttle, more = boost.
    pub quantum: u64,
    /// Thermal pressure level (0 = cool, 1 = warm, 2 = hot, 3 = critical).
    pub thermal_pressure: u8,
    /// Number of active prediction alarms.
    pub alarm_count: u8,
    /// System coherence (16.16 fixed-point). High = stable, low = unstable.
    pub coherence: Weight,
    /// Whether the twin has enough data to be authoritative.
    pub confident: bool,
}

/// Default hint when the twin is offline or has no data yet.
const SCHED_HINT_DEFAULT: SchedHint = SchedHint {
    quantum: crate::arch::timer::SCHED_TIME_SLICE_TICKS,
    thermal_pressure: 0,
    alarm_count: 0,
    coherence: WEIGHT_ONE,
    confident: false,
};

/// Query the twin for a scheduling hint.
///
/// Called from the scheduler's `schedule()` and `preempt()` paths.
/// Uses `try_lock()` to avoid blocking the dispatch path — if the
/// twin is locked (e.g., ingestion in progress), returns a default
/// "do nothing special" hint.
///
/// The hint encodes:
/// - **Thermal throttling**: if predicted CPU temp exceeds 80°C (80*65536),
///   reduce quantum proportionally. At 95°C, quantum drops to 2 ticks (20%).
/// - **Coherence boost**: if system is highly coherent (stable predictions),
///   allow extended quantum (up to 1.5×) because the twin is confident
///   nothing bad will happen.
/// - **Alarm response**: each active alarm further reduces quantum by 1 tick.
pub fn query_sched_hint() -> SchedHint {
    if !is_online() {
        return SCHED_HINT_DEFAULT;
    }

    let twin = match TWIN.try_lock() {
        Some(t) => t,
        None => {
            DROPPED_SAMPLES.fetch_add(1, Ordering::Relaxed);
            return SCHED_HINT_DEFAULT;
        }
    };

    // Need enough observations to be confident.
    let temp_idx = SENSOR_CPU_TEMP.load(Ordering::Acquire);
    let has_temp = temp_idx != SENSOR_NONE
        && twin
            .sensor(temp_idx as usize)
            .is_some_and(|s| s.observation_count() >= 8);

    let util_idx = SENSOR_CPU_UTIL.load(Ordering::Acquire);
    let has_util = util_idx != SENSOR_NONE
        && twin
            .sensor(util_idx as usize)
            .is_some_and(|s| s.observation_count() >= 8);

    if !has_temp && !has_util {
        return SchedHint {
            confident: false,
            coherence: twin.coherence(),
            alarm_count: twin.alarm_count().min(255) as u8,
            ..SCHED_HINT_DEFAULT
        };
    }

    // ── Thermal pressure ──────────────────────────────────────────
    // Predicted CPU temp at horizon 0 (next step).
    // Temperature thresholds in 16.16:
    //   70°C = 70 * 65536 = 4_587_520   (warm)
    //   80°C = 80 * 65536 = 5_242_880   (hot)
    //   90°C = 90 * 65536 = 5_898_240   (critical)
    const TEMP_WARM: Weight = 70 * WEIGHT_ONE;
    const TEMP_HOT: Weight = 80 * WEIGHT_ONE;
    const TEMP_CRITICAL: Weight = 90 * WEIGHT_ONE;

    let predicted_temp = if has_temp {
        let s = twin.sensor(temp_idx as usize).unwrap();
        if s.confidence() > WEIGHT_ONE / 4 {
            s.predictions()[0]
        } else {
            s.ema()
        }
    } else {
        0
    };

    let thermal_pressure = if predicted_temp >= TEMP_CRITICAL {
        3u8
    } else if predicted_temp >= TEMP_HOT {
        2u8
    } else if predicted_temp >= TEMP_WARM {
        1u8
    } else {
        0u8
    };

    // ── Time-slice calculation ────────────────────────────────────
    // Start with the default scheduler slice (10 ticks).
    let base_quantum: u64 = crate::arch::timer::SCHED_TIME_SLICE_TICKS;

    // Thermal scaling: reduce quantum under thermal pressure.
    //   pressure 0 → 100% (10)
    //   pressure 1 → 80%  (8)
    //   pressure 2 → 50%  (5)
    //   pressure 3 → 20%  (2)
    let thermal_quantum = match thermal_pressure {
        0 => base_quantum,
        1 => (base_quantum * 80) / 100,
        2 => (base_quantum * 50) / 100,
        _ => (base_quantum * 20) / 100,
    };

    // Coherence boost: if coherence > 0.8 (52429 in 16.16) and no
    // thermal pressure, extend quantum by up to 50%.
    let coherence = twin.coherence();
    let coherent = coherence > ((WEIGHT_ONE as u64 * 80) / 100) as Weight;
    let boosted_quantum = if thermal_pressure == 0 && coherent {
        // Boost proportional to coherence above 0.8.
        // At coherence=1.0, boost = 50% → quantum = 15.
        let boost_frac = coherence.saturating_sub(((WEIGHT_ONE as u64 * 80) / 100) as Weight);
        let boost = (base_quantum * boost_frac as u64 * 5) / (2 * WEIGHT_ONE as u64);
        thermal_quantum + boost.min(base_quantum / 2)
    } else {
        thermal_quantum
    };

    // Alarm penalty: subtract 1 tick per active alarm, floor at 2.
    let alarm_count = twin.alarm_count().min(MAX_ALARMS).min(255) as u8;
    let final_quantum = boosted_quantum.saturating_sub(alarm_count as u64).max(2);

    drop(twin);

    SchedHint {
        quantum: final_quantum,
        thermal_pressure,
        alarm_count,
        coherence,
        confident: true,
    }
}
