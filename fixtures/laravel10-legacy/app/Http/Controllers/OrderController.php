<?php

namespace App\Http\Controllers;

use App\Http\Requests\StoreOrderRequest;
use App\Models\Order;
use App\Services\OrderService;
use Illuminate\Http\JsonResponse;

class OrderController extends Controller
{
    public function __construct(private OrderService $orders)
    {
    }

    public function index(): JsonResponse
    {
        return response()->json(Order::paid()->get());
    }

    public function store(StoreOrderRequest $request): JsonResponse
    {
        return response()->json($this->orders->place($request->validated()), 201);
    }

    public function show(Order $order): JsonResponse
    {
        return response()->json($order->load('items'));
    }

    public function markPaid(Order $order): JsonResponse
    {
        return response()->json($this->orders->markPaid($order));
    }
}
