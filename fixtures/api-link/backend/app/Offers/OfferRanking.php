<?php

namespace App\Offers;

class OfferRanking
{
    public static function sort(iterable $offers): array
    {
        $list = is_array($offers) ? $offers : iterator_to_array($offers);
        usort($list, fn ($a, $b) => PriceRules::rounded($a->price) <=> PriceRules::rounded($b->price));

        return $list;
    }
}
