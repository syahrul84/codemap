<?php

if (! function_exists('money')) {
    /**
     * Formats an amount in cents as a money string.
     */
    function money(int $cents, string $currency = 'USD'): string
    {
        return $currency.' '.number_format($cents / 100, 2);
    }
}
