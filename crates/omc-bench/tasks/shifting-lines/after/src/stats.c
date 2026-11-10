/**
 * @file stats.c
 * @brief Running statistics over a list of samples.
 */

#include <stddef.h>
#include <math.h>

#include "stats.h"

/**
 * @brief Adds up the samples.
 * @param samples The samples.
 * @param count How many there are.
 * @return Their sum, or 0 when there are none.
 */
double stats_sum(const double *samples, size_t count) {
    double sum = 0.0;
    for (size_t i = 0; i < count; i++) {
        sum += samples[i];
    }
    return sum;
}

/**
 * @brief Averages the samples.
 * @param samples The samples.
 * @param count How many there are.
 * @return Their mean, or 0 when there are none.
 */
double stats_mean(const double *samples, size_t count) {
    if (count == 0) {
        return 0.0;
    }
    return stats_sum(samples, count) / (double)count;
}

/**
 * @brief Measures how far apart the smallest and largest samples are.
 * @param samples The samples.
 * @param count How many there are.
 * @return The largest sample less the smallest, or 0 when there are none.
 */
double stats_spread(const double *samples, size_t count) {
    if (count == 0) {
        return 0.0;
    }
    double low = samples[0];
    double high = samples[0];
    for (size_t i = 1; i < count; i++) {
        if (samples[i] < low) {
            low = samples[i];
        }
        if (samples[i] > high) {
            high = samples[i];
        }
    }
    return fabs(high - low);
}
