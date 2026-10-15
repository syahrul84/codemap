<?php

namespace App\Services;

use App\Models\Order;
use Illuminate\Support\Facades\DB;

/**
 * Places and settles customer orders.
 */
class OrderService
{
    /**
     * Creates an order with its items in one transaction.
     *
     * @param  array<string, mixed>  $data
     */
    public function place(array $data): Order
    {
        return DB::transaction(fn () => Order::create($data));
    }

    public function markPaid(Order $order): Order
    {
        $order->update(['status' => Order::STATUS_PAID, 'paid_at' => now()]);

        return $order;
    }
}
