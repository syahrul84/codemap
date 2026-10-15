<?php

return [
    'currency' => env('SHOP_CURRENCY', 'MYR'),
    'payments' => [
        'timeout' => (int) env('SHOP_PAYMENT_TIMEOUT', 30),
    ],
];
