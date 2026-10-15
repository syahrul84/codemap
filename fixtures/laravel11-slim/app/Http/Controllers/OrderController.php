<?php

namespace App\Http\Controllers;

use App\Http\Requests\StoreOrderRequest;
use App\Http\Requests\UpdateOrderRequest;
use App\Models\Order;

class OrderController extends Controller
{
    public function index()
    {
        return Order::query()->paid()->paginate();
    }

    public function store(StoreOrderRequest $request)
    {
        return Order::create($request->validated());
    }

    public function show(Order $order)
    {
        return $order->load('items', 'tags');
    }

    public function update(UpdateOrderRequest $request, Order $order)
    {
        $order->update($request->validated());

        return $order;
    }

    public function destroy(Order $order)
    {
        $order->delete();

        return response()->noContent();
    }
}
