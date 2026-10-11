# ADR-0010: Congestion control and quality ladder

**Status:** Proposed (validated in Phase 4 against recorded and synthetic network traces)

## Decision
* **Two layers.** QUIC's own congestion controller is a safety net; real-time quality is
  driven by an application-level **delay-gradient estimator** (trendline filter over frame
  inter-arrival vs send spacing, GCC-style) fed by receiver feedback, plus loss and queue
  signals from QUIC stats.
* **Ladder (reaction < 200 ms):** first bitrate, then frame rate (preferred for static
  content), then resolution scale (preferred for video content, detected from dirty-region
  statistics). Recovery steps up slowly and probes.
* **Loss:** reference-frame invalidation first, intra-refresh second, keyframe last resort.
* Pacing: frames are paced at send time; the encoder's VBV is sized from the current estimate.

## Validation plan
The estimator is pure logic, tested deterministically on synthetic traces (step down, ramp,
bufferbloat, random loss). Real-network behaviour requires the device lab; until then WAN
numbers are labelled *estimated*, not measured.
