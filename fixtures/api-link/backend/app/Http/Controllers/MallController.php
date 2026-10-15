<?php

namespace App\Http\Controllers;

use App\Http\Resources\MallResource;
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
}
