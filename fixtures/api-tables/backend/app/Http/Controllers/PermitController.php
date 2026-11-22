<?php

namespace App\Http\Controllers;

use Illuminate\Http\Request;

class PermitController extends Controller
{
    public function index()
    {
        return ['data' => []];
    }

    public function store(Request $request)
    {
        return ['data' => ['id' => 1, 'name' => $request->input('name')]];
    }

    public function show(int $permit)
    {
        return ['data' => ['id' => $permit]];
    }

    public function update(Request $request, int $visitor)
    {
        return ['data' => ['id' => $visitor, 'name' => $request->input('name')]];
    }

    public function destroy(int $permit)
    {
        return ['data' => ['id' => $permit]];
    }

    public function extensions(int $permit)
    {
        return ['data' => []];
    }

    public function extension(int $permit, int $extension)
    {
        return ['data' => ['id' => $extension]];
    }

    public function approvers()
    {
        return ['data' => []];
    }
}
