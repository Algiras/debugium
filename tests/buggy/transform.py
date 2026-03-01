"""Data transformation pipeline steps."""


def normalize(values: list[float], history: list = []) -> list[float]:
    """Normalize values to [0, 1]. Tracks history of min/max seen.

    BUG: mutable default argument — history is shared across ALL calls.
    After first call, min/max are polluted by previous runs.
    """
    history.extend(values)          # accumulates across calls forever
    lo = min(history)
    hi = max(history)

    if hi == lo:
        return [0.0] * len(values)

    return [(v - lo) / (hi - lo) for v in values]


def running_average(values: list[float]) -> list[float]:
    """Return the running average at each position."""
    result = []
    total = 0.0
    for i, v in enumerate(values):
        total += v
        result.append(total / (i + 1))
    return result


def threshold_filter(values: list[float], cutoff: float = 0.5) -> list[float]:
    """Keep only values above cutoff."""
    return [v for v in values if v > cutoff]
