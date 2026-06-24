/** API timestamps are UTC — display in Asia/Manila, 12-hour, no seconds. */

export const MANILA_TZ = "Asia/Manila";

export function parseAppTime(value) {
  if (value == null || value === "") return null;
  const d = value instanceof Date ? value : new Date(value);
  return Number.isFinite(d.getTime()) ? d : null;
}

export function fmtManilaTime(value) {
  const d = parseAppTime(value);
  if (!d) return "—";
  return d.toLocaleString("en-PH", {
    timeZone: MANILA_TZ,
    hour: "numeric",
    minute: "2-digit",
    hour12: true,
  });
}

export function fmtManilaDateTime(value) {
  const d = parseAppTime(value);
  if (!d) return "—";
  return d.toLocaleString("en-PH", {
    timeZone: MANILA_TZ,
    year: "numeric",
    month: "short",
    day: "numeric",
    hour: "numeric",
    minute: "2-digit",
    hour12: true,
  });
}

export function fmtManilaDate(value) {
  const d = parseAppTime(value);
  if (!d) return "—";
  return d.toLocaleDateString("en-PH", {
    timeZone: MANILA_TZ,
    year: "numeric",
    month: "short",
    day: "numeric",
  });
}
