<?php

namespace App\Http\Controllers;

use App\Http\Requests\StoreOfferRequest;
use Illuminate\Http\Request;

class OfferController extends Controller
{
    public function store(StoreOfferRequest $request)
    {
        return response()->json(['data' => $request->validated()]);
    }

    public function update(Request $request, int $offer)
    {
        $data = $request->validate([
            'title' => 'required|string',
            'note' => 'nullable|string',
        ]);

        return response()->json(['data' => $data]);
    }
}
