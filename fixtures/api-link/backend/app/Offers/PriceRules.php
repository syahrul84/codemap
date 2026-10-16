<?php

namespace App\Offers;

class PriceRules
{
    public static function rounded(float $price): float
    {
        return round($price, 2);
    }
}
