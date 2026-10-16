<?php

namespace App\Http\Controllers;

use App\Http\Resources\CartItemResource;
use Illuminate\Http\Request;

class CartController extends Controller
{
    public function index(Request $request)
    {
        $carts = $request->user()->carts()->get();

        return response()->json(['data' => $carts, 'count' => $carts->count()]);
    }

    public function add(Request $request, int $merchant)
    {
        $item = $request->user()->cartItems()->create($request->all());

        return new CartItemResource($item);
    }

    public function update(Request $request, int $merchant, int $item)
    {
        $line = $request->user()->cartItems()->findOrFail($item);
        $line->update($request->only('quantity'));

        return new CartItemResource($line);
    }

    public function remove(Request $request, int $merchant, int $item)
    {
        $request->user()->cartItems()->whereKey($item)->delete();

        return response()->noContent();
    }
}
