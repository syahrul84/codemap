<?php

namespace App\Support;

class Maintenance
{
    public static function wipeAllowed(): bool
    {
        // pidx-ignore: read at boot on purpose, before config is cached
        return (bool) env('ALLOW_DB_WIPE', false);
    }

    public static function state(bool $open): string
    {
        return $open ? __('pending') : __('Closed for the day').' '.__('validation.attributes.price');
    }

    public static function label(): string
    {
        return __('orders.archived') === 'orders.archived' ? 'Archived' : __('orders.archived');
    }
}
