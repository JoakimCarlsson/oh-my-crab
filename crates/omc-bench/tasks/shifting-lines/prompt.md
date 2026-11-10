Make these three changes to `src/stats.c`, and nothing else:

1. Add `#include <math.h>` as a new line directly after `#include <stddef.h>`.
2. Delete the whole function `stats_reset` together with the comment above it.
3. In `stats_spread`, change `return high - low;` to `return fabs(high - low);`.
