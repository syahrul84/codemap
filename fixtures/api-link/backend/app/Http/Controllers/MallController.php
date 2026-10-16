<?php

namespace App\Http\Controllers;

use App\Http\Resources\MallResource;
use App\Http\Resources\OfferResource;
use App\Models\Mall;

class MallController extends Controller
{
    public function index()
    {
        return MallResource::collection(Mall::query()->paginate());
    }

    public function show(Mall $mall)
    {
        return new MallResource($mall->load('offers'));
    }

    public function offers(Mall $mall)
    {
        return OfferResource::collection(\App\Offers\OfferRanking::sort($mall->offers()->get()));
    }
}
