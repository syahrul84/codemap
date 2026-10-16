export const UI_DEFAULTS = {
  checkIn: { en: "Check-in", ms: "Daftar masuk" },
  checkOut: { en: "Check-out", ms: "Daftar keluar" },
  guests: { en: "Guests", ms: "Tetamu" },
  night: { en: "night", ms: "malam" },
  total: { en: "Total", ms: "Jumlah" },
  book: { en: "Book", ms: "Tempah" },
};

export function t(key: keyof typeof UI_DEFAULTS): string {
  return UI_DEFAULTS[key].en;
}
