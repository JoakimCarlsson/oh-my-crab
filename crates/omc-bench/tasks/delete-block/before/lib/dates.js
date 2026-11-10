"use strict";

/**
 * Pads a number to two digits.
 * @param {number} value The number to pad.
 * @returns {string} The number, with a zero in front when it is below ten.
 */
function pad(value) {
  return String(value).padStart(2, "0");
}

/**
 * Formats a date as year, month and day.
 * @param {Date} date The date to format.
 * @returns {string} The date as `YYYY-MM-DD`.
 */
function formatDate(date) {
  return `${date.getFullYear()}-${pad(date.getMonth() + 1)}-${pad(date.getDate())}`;
}

/**
 * Formats a date the way the first version of the reports did.
 * @deprecated Use `formatDate`.
 * @param {Date} date The date to format.
 * @returns {string} The date as `DD/MM/YYYY`.
 */
function formatLegacy(date) {
  return `${pad(date.getDate())}/${pad(date.getMonth() + 1)}/${date.getFullYear()}`;
}

/**
 * Formats the time of a date.
 * @param {Date} date The date to format.
 * @returns {string} The time as `HH:MM`.
 */
function formatTime(date) {
  return `${pad(date.getHours())}:${pad(date.getMinutes())}`;
}

module.exports = {
  pad,
  formatDate,
  formatLegacy,
  formatTime,
};
