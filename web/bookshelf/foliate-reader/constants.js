/**
 * foliate-reader/constants.js — shared constants (Phase 1.2 split)
 */
export const READER_OVERLAY_ID = 'foliate-reader-overlay';
export const READER_VIEW_ID = 'foliate-reader-view';
export const READER_TOPBAR_ID = 'foliate-reader-topbar';
export const READER_BOTTOMBAR_ID = 'foliate-reader-bottombar';
export const READER_TITLE_ID = 'foliate-reader-title';
export const READER_PROGRESS_ID = 'foliate-reader-progress';
export const READER_SETTINGS_ID = 'foliate-reader-settings';
export const READER_TOC_ID = 'foliate-reader-toc';
export const READER_TOC_BACKDROP_ID = 'foliate-reader-toc-backdrop';
export const READER_SEARCH_ID = 'foliate-reader-search';
export const READER_SEARCH_RESULTS_ID = 'foliate-reader-search-results';
export const READER_ANNOTATION_MENU_ID = 'foliate-reader-annotation-menu';

export const LS_THEME = 'foliate:theme';
export const LS_FONTSIZE = 'foliate:fontSize';
export const LS_PROGRESS_PREFIX = 'readaware:progress:';

export const ANNOTATION_COLORS = {
  yellow: '#ffff00', green: '#00ff00', cyan: '#00ffff',
  pink: '#ff69b4', orange: '#ffa500',
};

export const THEMES = {
  light:   { bg: '#ffffff', text: '#333333', border: '#e0e0e0', toolbar: '#fafafa', accent: '#1a73e8' },
  dark:    { bg: '#1a1a1a', text: '#cccccc', border: '#333333', toolbar: '#2d2d2d', accent: '#8ab4f8' },
  sepia:   { bg: '#f4ecd8', text: '#5b4636', border: '#d4c9a8', toolbar: '#efe4c8', accent: '#8b6914' },
  parchment: { bg: '#fcf5e5', text: '#4a3c2a', border: '#e8dcc8', toolbar: '#f5edd5', accent: '#7a6230' },
};

export const FONT_SIZES = { small: 14, medium: 16, large: 18, xlarge: 20, huge: 24 };
export const FONT_FAMILIES = {
  serif: 'Georgia, "Noto Serif", "Source Han Serif SC", serif',
  sans:  '-apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, "Noto Sans", sans-serif',
  mono:  '"SF Mono", "Fira Code", "Fira Mono", "Courier New", monospace',
};
