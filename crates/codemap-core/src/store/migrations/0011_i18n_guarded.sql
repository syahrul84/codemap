-- Uses that supply their own text when the translation is missing
-- (`__('k') === 'k' ? 'Fallback' : __('k')`, `t('k', 'Fallback')`): never "undefined".
ALTER TABLE i18n_uses ADD COLUMN guarded INTEGER NOT NULL DEFAULT 0;
