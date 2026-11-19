<?php

namespace App\Http\Controllers;

use Illuminate\Http\Request;

class AssetController extends Controller
{
    public function index()
    {
        return ['data' => []];
    }

    public function store(Request $request)
    {
        return ['data' => ['id' => 1, 'name' => $request->input('name')]];
    }

    public function show(int $asset)
    {
        return ['data' => ['id' => $asset]];
    }

    public function update(Request $request, int $asset)
    {
        return ['data' => ['id' => $asset, 'name' => $request->input('name')]];
    }

    public function history(int $asset)
    {
        return ['data' => []];
    }

    public function categories()
    {
        return ['data' => []];
    }
}
