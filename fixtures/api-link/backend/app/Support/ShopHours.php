<?php

namespace App\Support;

use App\Models\Setting;

class ShopHours
{
    public const KEY_OPEN = 'shop.open_hour';

    public const DEFAULT_OPEN = 9;

    public static function open(): int
    {
        $stored = Setting::get(self::KEY_OPEN);

        return $stored === null ? self::DEFAULT_OPEN : (int) $stored;
    }

    public static function save(int $hour): void
    {
        Setting::put(self::KEY_OPEN, $hour);
    }
}
